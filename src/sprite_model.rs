//! 帧序列模型：从 `models/<模型名>/` 目录加载 PNG 动画帧并按状态播放。
//!
//! 目录规范（自制模型 / 预渲染 Live2D 导出帧的接入方式）：
//! ```text
//! models/示例猫/
//! ├── model.toml          # name / fps / [[costume]] name+dir / expressions
//! ├── default/            # 一套"换装"= 一个子目录
//! │   ├── idle_0.png idle_1.png ...   # 片段 = 同前缀 PNG 序列
//! │   ├── walk_0.png ...
//! │   └── expr_1_0.png ...            # 表情片段 expr_<表情下标>_<帧>
//! └── 冬装/ ...
//! ```
//! 片段缺失自动回退到 idle（爬墙回退 walk+旋转）。所有帧必须 64x64 RGBA PNG。
//!
//! 内存纪律：**懒加载 + 字节预算 LRU**。片段首次被渲染到才从磁盘解码
//! （单片段几毫秒，不卡 UI）；已解码总量超预算（默认 24MB）时按最久未用
//! 逐出整片段，再次用到会重新解码——Live2D 预渲染几百帧的大模型也不会
//! 内存爆炸。

use crate::model::{ModelInfo, PetModel, PetState, Pose};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// 已解码片段的字节预算（每帧 64*64*4 = 16KB）
const DEFAULT_BUDGET_BYTES: usize = 24 * 1024 * 1024;

/// 状态 → 片段名（模型目录里的文件前缀）。
/// Climb → "climb"：模型可提供预旋转的爬墙帧；缺失时渲染层回退
/// walk 帧并自动旋转 90°（见 render）。
pub fn clip_name(state: &PetState) -> &'static str {
    match state {
        PetState::Idle => "idle",
        PetState::Walk => "walk",
        PetState::Climb => "climb",
        PetState::Sleep => "sleep",
        PetState::Patted => "happy",
        PetState::Shocked | PetState::Thrown => "shock",
        PetState::Dragged => "dragged",
        PetState::Sitting | PetState::Perch => "sit",
        PetState::Stretch => "stretch",
        PetState::Groom => "groom",
        PetState::Eat => "eat",
    }
}

/// RGBA8 → 0xAARRGGBB
fn rgba_to_argb(rgba: &[u8]) -> Vec<u32> {
    rgba.chunks_exact(4)
        .map(|c| {
            ((c[3] as u32) << 24) | ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
        })
        .collect()
}

fn decode_png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let file = std::fs::File::open(path).ok()?;
    let decoder = png::Decoder::new(file);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    Some((info.width, info.height, buf[..info.buffer_size()].to_vec()))
}

/// 加载一个片段（同前缀 PNG 序列）；缺帧返回 None
fn load_clip(cdir: &Path, clip: &str) -> Option<Vec<Vec<u32>>> {
    let mut frames = Vec::new();
    let mut i = 0;
    while i < 64 {
        let Some((w, h, rgba)) = decode_png(&cdir.join(format!("{clip}_{i}.png"))) else {
            break;
        };
        if w != 64 || h != 64 {
            break;
        }
        frames.push(rgba_to_argb(&rgba));
        i += 1;
    }
    if frames.is_empty() { None } else { Some(frames) }
}

fn clip_bytes(clip: &[Vec<u32>]) -> usize {
    clip.iter().map(|f| f.len() * 4).sum()
}

/// 帧序列模型（PetModel 实现）：片段懒加载，解码量受预算约束
pub struct SpriteModel {
    pub dir: PathBuf,
    info: ModelInfo,
    fps: u64,
    costumes: Vec<String>,
    /// 每套换装的帧目录（懒加载时按需读盘）
    costume_dirs: Vec<PathBuf>,
    costume: usize,
    expr: usize,
    /// (片段名, 换装下标) → 解码缓存；None = 磁盘确认缺失（避免重复扫盘）。
    /// 换装缺失时由调用方回退到下标 0。
    cache: HashMap<(String, usize), Option<Vec<Vec<u32>>>>,
    /// 已加载片段的最后使用时刻（预算逐出依据）
    last_use: HashMap<(String, usize), Instant>,
    /// 当前已解码字节数
    decoded_bytes: usize,
    /// 解码预算（测试可调小）
    budget: usize,
    buf: Vec<u32>,
    /// 爬墙兜底旋转的输出缓冲（climb 片段缺失时用 walk 帧旋转）
    rot_buf: Vec<u32>,
}

impl SpriteModel {
    /// 从模型目录加载。只读 model.toml + 校验 idle_0.png 存在（一次 stat），
    /// 不做任何帧解码——切换模型零卡顿，片段在首次渲染时按需加载。
    pub fn load(dir: PathBuf) -> Option<Self> {
        let text = std::fs::read_to_string(dir.join("model.toml")).ok()?;
        let v = text.parse::<toml::Value>().ok()?;
        let name = v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("未命名模型")
            .to_string();
        let fps = v
            .get("fps")
            .and_then(|x| x.as_integer())
            .map(|x| x as u64)
            .unwrap_or(8)
            .clamp(1, 30);
        let size = v.get("size").and_then(|x| x.as_integer()).unwrap_or(64) as u32;
        if size != 64 {
            return None;
        }

        let mut costumes: Vec<(String, PathBuf)> = Vec::new();
        if let Some(arr) = v.get("costume").and_then(|x| x.as_array()) {
            for c in arr {
                let cn = c.get("name").and_then(|x| x.as_str()).unwrap_or("默认");
                let cd = c.get("dir").and_then(|x| x.as_str()).unwrap_or(".");
                costumes.push((cn.to_string(), dir.join(cd)));
            }
        }
        if costumes.is_empty() {
            costumes.push(("默认".into(), dir.clone()));
        }

        // idle 是所有缺失片段的回退兜底：第一套换装目录必须有 idle_0.png
        let cdir0 = costumes.first().map(|(_, d)| d.clone()).unwrap_or_else(|| dir.clone());
        if !cdir0.join("idle_0.png").exists() {
            return None;
        }

        let expressions: Vec<String> = v
            .get("expressions")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();

        Some(Self {
            dir,
            info: ModelInfo {
                name,
                costumes: costumes.iter().map(|(n, _)| n.clone()).collect(),
                expressions,
            },
            fps,
            costumes: costumes.iter().map(|(n, _)| n.clone()).collect(),
            costume_dirs: costumes.into_iter().map(|(_, d)| d).collect(),
            costume: 0,
            expr: 0,
            cache: HashMap::new(),
            last_use: HashMap::new(),
            decoded_bytes: 0,
            budget: DEFAULT_BUDGET_BYTES,
            buf: Vec::with_capacity(64 * 64),
            rot_buf: Vec::with_capacity(64 * 64),
        })
    }

    /// 确保片段已解码（缺失结果也会被缓存）。返回是否真实存在。
    fn ensure_clip(&mut self, name: &str, costume: usize) -> bool {
        let key = (name.to_string(), costume);
        if !self.cache.contains_key(&key) {
            let cdir = self
                .costume_dirs
                .get(costume)
                .cloned()
                .unwrap_or_else(|| self.dir.clone());
            let frames = load_clip(&cdir, name);
            let bytes = frames.as_ref().map(|c| clip_bytes(c)).unwrap_or(0);
            let present = frames.is_some();
            self.cache.insert(key.clone(), frames);
            if bytes > 0 {
                self.decoded_bytes += bytes;
                self.last_use.insert(key.clone(), Instant::now());
                self.evict_over_budget(&key);
            }
            return present;
        }
        self.cache.get(&key).map(|c| c.is_some()).unwrap_or(false)
    }

    /// 已解码总量超预算时，按最久未用逐出整片段（跳过刚加载的 keep）
    fn evict_over_budget(&mut self, keep: &(String, usize)) {
        while self.decoded_bytes > self.budget {
            let victim = self
                .last_use
                .iter()
                .filter(|(k, _)| k != &keep)
                .filter(|(k, _)| self.cache.get(*k).map(|c| c.is_some()).unwrap_or(false))
                .min_by_key(|(_, t)| *t)
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else { break };
            if let Some(Some(clip)) = self.cache.remove(&victim) {
                self.decoded_bytes -= clip_bytes(&clip);
            }
            self.last_use.remove(&victim);
        }
    }

    /// 片段可用性（当前换装优先，缺失回退第 0 套；触发懒加载）
    fn has_clip(&mut self, name: &str) -> bool {
        let c = self.costume;
        self.ensure_clip(name, c) || self.ensure_clip(name, 0)
    }

    /// 取片段帧：当前换装优先，缺失回退第 0 套；刷新 LRU 时间戳。
    /// 返回实际命中的换装下标。
    fn frames_of(&mut self, name: &str) -> Option<usize> {
        let mut costume = self.costume;
        if !self.ensure_clip(name, costume) {
            costume = 0;
            self.ensure_clip(name, 0);
        }
        let key = (name.to_string(), costume);
        let present = self.cache.get(&key).map(|c| c.is_some()).unwrap_or(false);
        if present {
            self.last_use.insert(key, Instant::now());
            Some(costume)
        } else {
            None
        }
    }
}

impl PetModel for SpriteModel {
    fn info(&self) -> &ModelInfo {
        &self.info
    }

    fn frame_ms(&self, _state: &PetState) -> u64 {
        1000 / self.fps
    }

    fn render(&mut self, pose: &Pose) -> &[u32] {
        // 钉选表情且有对应片段 → 循环播放表情动画
        let expr_name = if pose.expr > 0 { format!("expr_{}", pose.expr) } else { String::new() };
        // 爬墙：优先模型自带的预旋转 climb 片段；缺失回退 walk 帧 + 旋转兜底
        // （内置模型爬墙是旋转的，帧序列模型不能直立贴墙）
        let use_climb_fallback =
            pose.state == PetState::Climb && !self.has_clip("climb");
        let state_name = if use_climb_fallback {
            "walk".to_string()
        } else {
            clip_name(&pose.state).to_string()
        };
        let name = if !expr_name.is_empty() && self.has_clip(&expr_name) {
            expr_name
        } else {
            state_name
        };
        if let Some(costume) = self.frames_of(&name) {
            let key = (name.clone(), costume);
            if let Some(Some(clip)) = self.cache.get(&key) {
                let f = &clip[pose.tick as usize % clip.len()];
                self.buf.clear();
                self.buf.extend_from_slice(f);
            }
        }
        // 爬墙兜底旋转：仅当实际播放的是 walk 帧（climb 片段视为已预旋转）
        if pose.state == PetState::Climb && pose.aux != 0 && name == "walk" {
            crate::sprites::rotate90_into(&mut self.rot_buf, &self.buf, pose.aux < 0, 64);
            return &self.rot_buf;
        }
        &self.buf
    }

    fn set_costume(&mut self, idx: usize) {
        self.costume = idx.min(self.costumes.len().saturating_sub(1));
    }
    fn set_expression(&mut self, idx: usize) {
        self.expr = idx;
    }
    fn costume(&self) -> usize {
        self.costume
    }
    fn expression(&self) -> usize {
        self.expr
    }
}

/// 扫描 models/ 根目录下的模型：(显示名, 模型目录)。
/// 结果按显示名稳定排序——模型注册表下标会被持久化（settings.model_kind），
/// 目录枚举顺序在 Windows 上无保证，不排序会导致重启后恢复到错误的模型。
pub fn discover(base: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(base) else {
        return out;
    };
    let dirs: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("model.toml").exists())
        .collect();
    for dir in dirs {
        let name = std::fs::read_to_string(dir.join("model.toml"))
            .ok()
            .and_then(|t| t.parse::<toml::Value>().ok())
            .and_then(|v| v.get("name").and_then(|x| x.as_str()).map(String::from))
            .unwrap_or_else(|| dir.file_name().unwrap().to_string_lossy().to_string());
        out.push((name, dir));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    out
}

/// 把 ARGB 帧编码为 PNG（导出示例模型用）
pub fn encode_png(path: &Path, w: u32, h: u32, argb: &[u32]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    let mut rgba = Vec::with_capacity(argb.len() * 4);
    for &p in argb {
        rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
    }
    writer.write_image_data(&rgba).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端到端：models/示例猫（由 export_example_model 导出并随仓库分发）
    /// 必须能被 discover 发现、加载成功，且渲染出非空帧
    #[test]
    fn example_model_loads_and_renders() {
        let base = std::path::Path::new("models");
        let found = discover(base);
        assert!(!found.is_empty(), "models/ 下应至少有示例模型");
        let Some((_, dir)) = found.iter().find(|(n, _)| n == "示例猫") else {
            panic!("示例猫模型缺失");
        };
        let mut model = SpriteModel::load(dir.clone()).expect("示例猫应加载成功");
        assert!(model.info.costumes.len() >= 1);
        let pose = Pose {
            state: PetState::Idle,
            tick: 0,
            gaze: (0, 0),
            expr: 0,
            costume: 0,
            aux: 0,
            typing: false,
        };
        let out = model.render(&pose);
        assert_eq!(out.len(), 64 * 64);
        assert!(out.iter().any(|&p| p != 0), "渲染帧不能全透明");
    }

    /// 回归：无 climb 片段的模型爬墙时必须旋转 walk 帧兜底，
    /// 不能直立贴墙（曾经 Climb 直接映射 walk 直立播放）。
    /// 用临时目录确定性构造（示例猫的 climb 片段是旧导出的直立帧，语义不可靠）。
    #[test]
    fn climb_without_clip_rotates_walk_frame() {
        let dir = std::env::temp_dir().join(format!("deskpet_climb_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.toml"), "name = \"climb_t\"\n").unwrap();
        // 非对称帧（左上角有色块）：旋转后与原图不同，纯色帧旋转等于自身
        let mut frame = vec![0u32; 64 * 64];
        for y in 0..16 {
            for x in 0..16 {
                frame[y * 64 + x] = 0xFF112233;
            }
        }
        encode_png(&dir.join("idle_0.png"), 64, 64, &frame).unwrap();
        encode_png(&dir.join("walk_0.png"), 64, 64, &frame).unwrap();
        let mut model = SpriteModel::load(dir.clone()).expect("应加载成功");
        assert!(!model.has_clip("climb"), "夹具不应含 climb 片段");

        let walk = Pose { state: PetState::Walk, tick: 0, gaze: (0, 0), expr: 0, costume: 0, aux: 0, typing: false };
        let upright = model.render(&walk).to_vec();
        let climb = model.render(&Pose { state: PetState::Climb, aux: -1, ..walk });
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(climb.len(), 64 * 64);
        assert_ne!(upright.as_slice(), climb, "爬墙帧应是旋转后的 walk 帧");
        // 空白帧旋转后仍非空
        assert!(climb.iter().any(|&p| p != 0), "旋转兜底不能输出全透明帧");
    }

    /// 懒加载 + 预算逐出：超预算时最久未用的片段被逐出，再次用到重新解码
    #[test]
    fn lazy_loading_evicts_over_budget() {
        let dir = std::env::temp_dir().join(format!("deskpet_lru_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.toml"), "name = \"lru\"\nfps = 8\n").unwrap();
        // 每帧 64*64*4 = 16KB：idle 2 帧 + walk 6 帧 = 128KB
        let blank = vec![0xFF112233u32; 64 * 64];
        for i in 0..2 {
            encode_png(&dir.join(format!("idle_{i}.png")), 64, 64, &blank).unwrap();
        }
        for i in 0..6 {
            encode_png(&dir.join(format!("walk_{i}.png")), 64, 64, &blank).unwrap();
        }
        let mut model = SpriteModel::load(dir.clone()).expect("应加载成功");
        model.budget = 100 * 1024; // 100KB < 128KB：装不下全部

        // 先播 walk（装载 walk，96KB）
        let walk = Pose { state: PetState::Walk, tick: 0, gaze: (0, 0), expr: 0, costume: 0, aux: 0, typing: false };
        let w = model.render(&walk);
        assert!(w.iter().any(|&p| p != 0));
        assert!(model.cache.get(&("walk".into(), 0)).unwrap().is_some());
        // 再播 idle（32KB → 总 128KB 超预算 → walk 被逐出）
        let idle = Pose { state: PetState::Idle, tick: 0, gaze: (0, 0), expr: 0, costume: 0, aux: 0, typing: false };
        let i = model.render(&idle);
        assert!(i.iter().any(|&p| p != 0));
        assert!(
            model.cache.get(&("walk".into(), 0)).map(|c| c.is_none()).unwrap_or(true),
            "超预算后 walk 片段应被逐出"
        );
        // 再次播 walk：应重新从磁盘解码成功
        let w2 = model.render(&walk);
        assert!(w2.iter().any(|&p| p != 0), "逐出后再次使用应重新解码");
        assert!(model.cache.get(&("walk".into(), 0)).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 缺失片段的结果被缓存（None），不会每次渲染都扫盘
    #[test]
    fn missing_clip_result_is_cached() {
        let dir = std::env::temp_dir().join(format!("deskpet_miss_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.toml"), "name = \"miss\"\n").unwrap();
        let blank = vec![0xFF445566u32; 64 * 64];
        encode_png(&dir.join("idle_0.png"), 64, 64, &blank).unwrap();
        let mut model = SpriteModel::load(dir.clone()).unwrap();
        assert!(!model.ensure_clip("climb", 0), "磁盘上没有 climb 片段");
        assert!(model.cache.contains_key(&("climb".into(), 0)), "缺失结果应缓存为 None");
        assert!(!model.ensure_clip("climb", 0), "第二次查询直接命中缓存，不再扫盘");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
