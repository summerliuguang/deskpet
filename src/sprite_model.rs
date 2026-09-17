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
//! 片段缺失自动回退到 idle。所有帧必须 64x64 RGBA PNG。

use crate::model::{ModelInfo, PetModel, PetState, Pose};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// 帧序列模型（PetModel 实现）：数据全部在加载时读入内存
pub struct SpriteModel {
    pub dir: PathBuf,
    info: ModelInfo,
    fps: u64,
    costumes: Vec<String>,
    costume: usize,
    expr: usize,
    /// (片段名, 换装下标) → 动画帧；换装缺失时回退到下标 0
    clips: HashMap<(String, usize), Vec<Vec<u32>>>,
    buf: Vec<u32>,
    /// 爬墙兜底旋转的输出缓冲（climb 片段缺失时用 walk 帧旋转）
    rot_buf: Vec<u32>,
}

impl SpriteModel {
    /// 从模型目录加载；缺 idle 片段（所有缺失片段的回退兜底）或尺寸不符时返回 None
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

        let expressions: Vec<String> = v
            .get("expressions")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let mut clips = HashMap::new();
        let clip_names = [
            "idle", "walk", "sleep", "happy", "shock", "dragged", "sit", "stretch", "groom",
            "eat", "climb", "perch",
        ];
        for (ci, (_, cdir)) in costumes.iter().enumerate() {
            for cn in clip_names {
                if let Some(frames) = load_clip(cdir, cn) {
                    clips.insert((cn.to_string(), ci), frames);
                }
            }
            for ei in 0..expressions.len() {
                if let Some(frames) = load_clip(cdir, &format!("expr_{ei}")) {
                    clips.insert((format!("expr_{ei}"), ci), frames);
                }
            }
        }

        // idle 是所有缺失片段的回退兜底，必须有
        clips.get(&("idle".into(), 0))?;

        Some(Self {
            dir,
            info: ModelInfo {
                name,
                costumes: costumes.iter().map(|(n, _)| n.clone()).collect(),
                expressions,
            },
            fps,
            costumes: costumes.into_iter().map(|(n, _)| n).collect(),
            costume: 0,
            expr: 0,
            clips,
            buf: Vec::with_capacity(64 * 64),
            rot_buf: Vec::with_capacity(64 * 64),
        })
    }

    fn clip_len(&self, name: &str) -> usize {
        self.clips
            .get(&(name.to_string(), self.costume))
            .or_else(|| self.clips.get(&(name.to_string(), 0)))
            .map(|c| c.len())
            .unwrap_or(0)
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
        let expr_clip = if pose.expr > 0 {
            Some(format!("expr_{}", pose.expr))
        } else {
            None
        };
        // 爬墙：优先用模型自带的预旋转 climb 片段；缺失则回退 walk 帧
        // 并旋转 90° 兜底（内置模型爬墙是旋转的，帧序列模型不能直立贴墙）
        let state_clip = if pose.state == PetState::Climb && self.clip_len("climb") == 0 {
            "walk".to_string()
        } else {
            clip_name(&pose.state).to_string()
        };
        let name = match expr_clip {
            Some(c) if self.clip_len(&c) > 0 => c,
            _ => state_clip,
        };
        let len = self.clip_len(&name);
        if len > 0 {
            // clips 与 buf 是不同字段，字段级借用可分离
            if let Some(clip) = self
                .clips
                .get(&(name.clone(), self.costume))
                .or_else(|| self.clips.get(&(name.clone(), 0)))
            {
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

    /// discover 结果按显示名稳定排序（注册表下标被持久化，顺序乱会恢复错模型）
    #[test]
    fn discover_results_sorted_by_name() {
        let base = std::env::temp_dir().join(format!("deskpet_discover_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for name in ["乙模型", "甲模型"] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("model.toml"), format!("name = \"{name}\"\n")).unwrap();
        }
        let found = discover(&base);
        let _ = std::fs::remove_dir_all(&base);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "discover 应按显示名排序: {names:?}");
    }

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
    #[test]
    fn climb_without_clip_rotates_walk_frame() {
        let base = std::path::Path::new("models");
        let found = discover(base);
        let Some((_, dir)) = found.iter().find(|(n, _)| n == "示例猫") else {
            panic!("示例猫模型缺失");
        };
        let mut model = SpriteModel::load(dir.clone()).expect("示例猫应加载成功");
        if model.clips.contains_key(&("climb".to_string(), 0)) {
            return; // 模型自带预旋转 climb 片段，走另一条路径
        }
        let walk = Pose {
            state: PetState::Walk,
            tick: 0,
            gaze: (0, 0),
            expr: 0,
            costume: 0,
            aux: 0,
            typing: false,
        };
        let upright = model.render(&walk).to_vec();
        let climb = model.render(&Pose { state: PetState::Climb, aux: -1, ..walk });
        assert_eq!(climb.len(), 64 * 64);
        assert_ne!(upright.as_slice(), climb, "爬墙帧应是旋转后的 walk 帧");
    }
}
