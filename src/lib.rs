pub mod ai;
pub mod bubble;
pub mod inputbox;
pub mod config;
pub mod input;
pub mod menu;
pub mod model;
pub mod sprite_model;
pub mod sprites;
pub mod text;
pub mod todo;
pub mod tts;

/// 事件循环用户事件
#[derive(Debug, Clone)]
pub enum PetEvent {
    /// 前台全屏状态变化
    FullscreenChanged(bool),
    /// AI 回复到达
    ChatReply(Result<String, String>),
    /// 全局键盘有按键（打字联动）
    Typing,
    /// 手柄按键
    Gamepad,
    /// 全局快捷键（Ctrl+Shift + D勿扰/T待办/C聊天/H隐藏/Q退出）
    Hotkey(u32),
    /// 显示器拓扑变化（热插拔/分辨率改变）
    MonitorsChanged,
}

pub type PetEventProxy = winit::event_loop::EventLoopProxy<PetEvent>;

/// 显示器矩形（多屏适配）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl MonRect {
    pub fn contains_point(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    pub fn contains_center(&self, px: i32, py: i32, size: i32) -> bool {
        let (cx, cy) = (px + size / 2, py + size / 2);
        cx >= self.x && cx < self.x + self.w && cy >= self.y && cy < self.y + self.h
    }

    /// 带容差的中心判定：宠物中心在显示器外扩 tol 后仍算在屏内。
    /// 跨屏拖动/飞行落点贴近边缘时避免"突跳归属"到另一块屏。
    pub fn contains_center_tol(&self, px: i32, py: i32, size: i32, tol: i32) -> bool {
        let (cx, cy) = (px + size / 2, py + size / 2);
        let (x1, y1) = (self.x - tol, self.y - tol);
        let (x2, y2) = (self.x + self.w + tol, self.y + self.h + tol);
        cx >= x1 && cx < x2 && cy >= y1 && cy < y2
    }
}

/// UI 缩放系数 ×256 定点存储。
/// 用原子量而不是 OnceLock：OnceLock 只能 set 一次，跨 DPI 屏拖动时
/// ScaleFactorChanged 的第二次设置会被静默吞掉（缩放永远锁死在启动值）。
static UI_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(256);

pub fn set_ui_scale(s: f64) {
    use std::sync::atomic::Ordering;
    let q = (s.max(0.5) * 256.0).round() as u32;
    UI_SCALE.store(q.max(128), Ordering::Relaxed);
}

pub fn ui_scale() -> f64 {
    use std::sync::atomic::Ordering;
    UI_SCALE.load(std::sync::atomic::Ordering::Relaxed) as f64 / 256.0
}

/// 逻辑尺寸 → 物理像素
pub fn ui(v: i32) -> i32 {
    (v as f64 * ui_scale()).round() as i32
}

/// 毫秒级 UNIX 时间戳（供节流/计时的小工具）
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 启动/运行诊断日志：写到 exe 旁 deskpet.log（不可写则退 %TEMP%）。
/// 放在 lib 层：config/todo 等库模块也需要记录失败。
pub fn dlog(msg: &str) {
    append_log(&format!("[{}] {msg}", now_ms() / 1000));
}

/// 带限频的警告日志：同 key 60 秒内只记一次，防止高频失败（如每帧
/// present 失败）刷爆日志文件。
pub fn dwarn(key: &str, msg: &str) {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static LAST_WARN: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
    let now = now_ms();
    let Ok(mut guard) = LAST_WARN.lock() else { return };
    let map = guard.get_or_insert_with(HashMap::new);
    if map.get(key).map(|t| now.saturating_sub(*t) < 60_000).unwrap_or(false) {
        return;
    }
    map.insert(key.to_string(), now);
    drop(guard);
    append_log(&format!("[{}][WARN] {msg}", now / 1000));
}

fn append_log(line: &str) {
    use std::io::Write;
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("deskpet.log")))
        .unwrap_or_else(|| std::env::temp_dir().join("deskpet.log"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// softbuffer 0.4 的泛型参数直接持有窗口句柄，用 Arc<Window> 保证 'static
pub type SbSurface =
    softbuffer::Surface<std::sync::Arc<winit::window::Window>, std::sync::Arc<winit::window::Window>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monrect_contains_center() {
        let m = MonRect { x: 100, y: 0, w: 800, h: 600 };
        assert!(m.contains_center(400, 300, 64));
        assert!(!m.contains_center(0, 0, 64), "左屏外的点不应命中");
    }

    #[test]
    fn monrect_contains_center_with_tolerance() {
        let m = MonRect { x: 0, y: 0, w: 800, h: 600 };
        let size = 64;
        // 中心在右缘外 8px（< tol=16）：带容差命中，无容差不命中
        let outside_x = 800 + 8 - size / 2;
        assert!(!m.contains_center(outside_x, 300, size));
        assert!(m.contains_center_tol(outside_x, 300, size, size / 4));
        // 中心在外 40px（> tol）：带容差也不命中
        let far_x = 800 + 40 - size / 2;
        assert!(!m.contains_center_tol(far_x, 300, size, size / 4));
    }

    /// 回归：UI_SCALE 必须可反复设置。曾用 OnceLock 只能 set 一次，
    /// 跨 DPI 屏拖动时第二次设置被静默吞掉，缩放永远锁死在启动值。
    #[test]
    fn ui_scale_is_settable_repeatedly() {
        set_ui_scale(1.5);
        assert!((ui_scale() - 1.5).abs() < 1e-9);
        set_ui_scale(1.0);
        assert!((ui_scale() - 1.0).abs() < 1e-9);
        // 下限保护：0.2 会被夹到 0.5
        set_ui_scale(0.2);
        assert!((ui_scale() - 0.5).abs() < 1e-9);
        set_ui_scale(1.0);
    }
}
