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
}

pub type PetEventProxy = winit::event_loop::EventLoopProxy<PetEvent>;

/// 显示器矩形（多屏适配）
#[derive(Debug, Clone, Copy)]
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
}

/// UI 缩放系数（主显示器 scale_factor，resumed 时设置；未设置 = 1.0）
static UI_SCALE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();

pub fn set_ui_scale(s: f64) {
    let _ = UI_SCALE.set(s.max(0.5));
}

pub fn ui_scale() -> f64 {
    *UI_SCALE.get().unwrap_or(&1.0)
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
}
