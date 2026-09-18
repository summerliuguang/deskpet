//! 悬浮输入框：贴在宠物下方的小输入条（像素风），回车发送、Esc 关闭。
//! 拖图片进来直接触发识别。回复以气泡形式出现在猫头顶（见 main.rs / bubble.rs）。

use crate::text::{base_px, ui, Rgb, TEXT};
use crate::SbSurface;
use std::{num::NonZeroU32, sync::Arc};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, WindowEvent},
    event_loop::ActiveEventLoop,
    window::Window,
};

const W: i32 = 252;
const H: i32 = 34;

const BG: u32 = 0xFFFDF9EE;
const BORDER: u32 = 0xFF5A5040;
const INK: Rgb = (45, 42, 38);
const GRAY: Rgb = (155, 148, 134);

pub enum InputAction {
    None,
    Send { text: String, image: Option<String> },
    Close,
}

pub struct InputBox {
    pub window: Arc<Window>,
    surface: SbSurface,
    pub input: String,
    preedit: Option<String>,
    pub pending: bool,
    placeholder: String,
    /// 本次会话发送过的消息（↑/↓ 翻阅）
    sent_history: Vec<String>,
    /// 历史翻阅位置：None = 不在翻阅状态（输入区与末尾对齐）
    hist_pos: Option<usize>,
    w: i32,
    h: i32,
}

impl InputBox {
    pub fn create(el: &ActiveEventLoop, placeholder: String) -> Option<Self> {
        // 窗口与 surface 必须同用物理尺寸（逻辑值经 ui() 缩放），否则高 DPI 下右侧被裁
        let (w, h) = (ui(W), ui(H));
        #[allow(unused_mut)] // windows 下会追加 with_skip_taskbar
        let mut attrs = Window::default_attributes()
            .with_inner_size(PhysicalSize::new(w as u32, h as u32))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_title("deskpet-input")
            .with_visible(false);
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let window = Arc::new(el.create_window(attrs).ok()?);
        window.set_ime_allowed(true);
        let ctx = softbuffer::Context::new(window.clone()).ok()?;
        let mut surface = softbuffer::Surface::new(&ctx, window.clone()).ok()?;
        surface
            .resize(NonZeroU32::new(w as u32).unwrap(), NonZeroU32::new(h as u32).unwrap())
            .ok()?;
        Some(Self {
            window,
            surface,
            input: String::new(),
            preedit: None,
            pending: false,
            placeholder,
            sent_history: Vec::new(),
            hist_pos: None,
            w,
            h,
        })
    }

    /// 记录一条已发送消息（↑/↓ 翻阅用，上限 50 条）
    pub fn push_history(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.sent_history.push(text);
        if self.sent_history.len() > 50 {
            self.sent_history.remove(0);
        }
        self.hist_pos = None;
    }

    /// ↑/↓ 在已发送历史中翻阅；IME 组字中不拦截
    fn browse_history(&mut self, up: bool) {
        if self.sent_history.is_empty() || self.preedit.is_some() {
            return;
        }
        let len = self.sent_history.len();
        let pos = self.hist_pos.unwrap_or(len);
        let new_pos = if up {
            pos.saturating_sub(1)
        } else {
            (pos + 1).min(len) // 越过最新一条 = 回到空输入
        };
        if new_pos >= len {
            self.hist_pos = None;
            self.input.clear();
        } else {
            self.hist_pos = Some(new_pos);
            self.input = self.sent_history[new_pos].clone();
        }
        self.draw();
    }

    /// 贴着宠物下方打开；贴不下（猫在屏幕底缘）就放猫上方
    pub fn open(&mut self, pet_pos: (i32, i32), pet_size: i32, mon: crate::MonRect) {
        let x = (pet_pos.0 + pet_size / 2 - self.w / 2)
            .clamp(mon.x + 4, (mon.x + mon.w - self.w - 4).max(mon.x + 4));
        let below = pet_pos.1 + pet_size + ui(6);
        let y = if below + self.h <= mon.y + mon.h - ui(2) {
            below
        } else {
            (pet_pos.1 - self.h - ui(6)).max(mon.y + ui(2))
        };
        self.window.set_outer_position(PhysicalPosition::new(x, y));
        self.window.set_visible(true);
        self.window.focus_window();
        // winit 的 focus 可能被前台锁定拒绝（桌宠进程非前台），强制补一次
        if let Some(h) = crate::hwnd_of(&self.window) {
            crate::force_focus_window(h);
        }
        self.draw();
    }

    pub fn set_pending(&mut self) {
        self.pending = true;
        self.draw();
    }

    pub fn clear_pending(&mut self) {
        self.pending = false;
        self.draw();
    }

    /// 处理输入框自己的窗口事件，返回需要 App 层执行的动作
    pub fn handle(&mut self, event: &WindowEvent) -> InputAction {
        match event {
            WindowEvent::RedrawRequested => {
                self.draw();
                InputAction::None
            }
            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::{Key, NamedKey};
                if event.state != ElementState::Pressed {
                    return InputAction::None;
                }
                match event.logical_key {
                    Key::Named(NamedKey::Enter) => {
                        let text = self.input.trim().to_string();
                        if text.is_empty() {
                            return InputAction::None;
                        }
                        self.input.clear();
                        self.preedit = None;
                        self.hist_pos = None;
                        InputAction::Send { text, image: None }
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.browse_history(true);
                        InputAction::None
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.browse_history(false);
                        InputAction::None
                    }
                    Key::Named(NamedKey::Backspace) => {
                        self.input.pop();
                        self.draw();
                        InputAction::None
                    }
                    Key::Named(NamedKey::Escape) => InputAction::Close,
                    _ => InputAction::None,
                }
            }
            WindowEvent::Ime(Ime::Commit(s)) => {
                self.input.push_str(s);
                self.preedit = None;
                self.draw();
                InputAction::None
            }
            WindowEvent::Ime(Ime::Preedit(s, _)) => {
                self.preedit = if s.is_empty() { None } else { Some(s.clone()) };
                self.draw();
                InputAction::None
            }
            WindowEvent::DroppedFile(path) => self.handle_drop(path),
            WindowEvent::CloseRequested => InputAction::Close,
            _ => InputAction::None,
        }
    }

    fn handle_drop(&mut self, path: &std::path::Path) -> InputAction {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            _ => {
                self.input = "（这个类型读不了，拖 png/jpg/webp/gif 给我）".into();
                self.draw();
                return InputAction::None;
            }
        };
        if std::fs::metadata(path).map(|m| m.len() > 4 * 1024 * 1024).unwrap_or(true) {
            self.input = "（图片超过 4MB，读不动喵）".into();
            self.draw();
            return InputAction::None;
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                self.input = "（文件读不到喵）".into();
                self.draw();
                return InputAction::None;
            }
        };
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        InputAction::Send {
            text: "请看看这张图片，用中文简短描述一下～".into(),
            image: Some(format!("data:{mime};base64,{b64}")),
        }
    }

    pub fn draw(&mut self) {
        let px = base_px();
        let mut buf = vec![BG; (self.w * self.h) as usize];
        // 像素风边框（厚度随缩放）
        let b = ui(2).max(1);
        for y in 0..self.h {
            for x in 0..self.w {
                let edge = x < b || x >= self.w - b || y < b || y >= self.h - b;
                buf[(y * self.w + x) as usize] = if edge { BORDER } else { BG };
            }
        }
        let shown: String = if self.pending {
            "思考中…".to_string()
        } else {
            format!("{}{}", self.input, self.preedit.as_deref().unwrap_or(""))
        };
        let color = if self.pending || (self.input.is_empty() && self.preedit.is_none()) {
            GRAY
        } else {
            INK
        };
        let shown = if self.pending {
            shown
        } else if shown.is_empty() {
            self.placeholder.clone()
        } else {
            shown
        };
        let pad_l = ui(8);
        // 超宽只显示尾部
        let full_count = shown.chars().count();
        let mut clipped = shown.clone();
        while TEXT.text_width(&clipped, px) > self.w - ui(20) && !clipped.is_empty() {
            let mut it = clipped.chars();
            it.next();
            clipped = it.as_str().to_string();
        }
        let hidden_head = full_count - clipped.chars().count();
        TEXT.draw_clipped(
            &mut buf, self.w, self.h,
            (ui(5), ui(4), self.w - ui(5), self.h - ui(4)),
            px, pad_l, ui(4), &[clipped], color, false,
        );
        // 光标（非思考中才显示）
        if !self.pending {
            let shown_full: String = format!("{}{}", self.input, self.preedit.as_deref().unwrap_or(""));
            let vis: String = shown_full.chars().skip(hidden_head).collect();
            let caret_x = (pad_l + TEXT.text_width(&vis, px)).min(self.w - ui(10));
            for y in ui(7)..self.h - ui(7) {
                buf[(y * self.w + caret_x) as usize] = 0xFF3A3644;
            }
        }
        if let Ok(mut b) = self.surface.buffer_mut() {
            for (dst, src) in b.iter_mut().zip(buf.iter()) {
                *dst = *src;
            }
            let _ = b.present();
        }
    }
}
