//! 气泡对话框：独立的无边框透明小窗，点击穿透（set_cursor_hittest(false)），
//! 显示在猫头顶，到时自动消失。纯软件渲染中文文本。

use crate::text::{Rgb, TEXT};
use crate::SbSurface;
use std::{num::NonZeroU32, sync::Arc};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event_loop::ActiveEventLoop,
    window::{Window, WindowLevel},
};

const INK: Rgb = (45, 42, 38);
const PAPER: u32 = 0xFFFDF9EE;
const BORDER: u32 = 0xFF5A5040;

pub struct BubbleWin {
    pub window: Arc<Window>,
    surface: SbSurface,
    lines: Vec<String>,
    size: (i32, i32),
    above: bool,
}

impl BubbleWin {
    pub fn create(el: &ActiveEventLoop) -> Option<Self> {
        #[allow(unused_mut)] // windows 下会追加 with_skip_taskbar
        let mut attrs = Window::default_attributes()
            .with_inner_size(PhysicalSize::new(60u32, 40u32))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_active(false)
            .with_visible(false)
            .with_title("bubble");
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let window = Arc::new(el.create_window(attrs).ok()?);
        // 整个气泡永远不挡鼠标
        let _ = window.set_cursor_hittest(false);
        let ctx = softbuffer::Context::new(window.clone()).ok()?;
        let mut surface = softbuffer::Surface::new(&ctx, window.clone()).ok()?;
        surface
            .resize(NonZeroU32::new(60).unwrap(), NonZeroU32::new(40).unwrap())
            .ok()?;
        Some(Self { window, surface, lines: Vec::new(), size: (60, 40), above: true })
    }

    /// 显示气泡。above=true 时尾巴朝下（气泡在猫头顶）。
    pub fn show(&mut self, text: &str, pet_pos: (i32, i32), mon: crate::MonRect, above: bool) {
        let layout = TEXT.layout(text, 200);
        let w = (layout.width + 24).clamp(56, 240);
        let h = layout.height + 12 + 6;
        self.size = (w, h);
        self.lines = layout.lines;
        self.above = above;
        let _ = self.window.request_inner_size(PhysicalSize::new(w as u32, h as u32));
        let bx = (pet_pos.0 + 32 - w / 2).clamp(mon.x + 4, (mon.x + mon.w - w - 4).max(mon.x + 4));
        let by = if above { pet_pos.1 - h - 8 } else { pet_pos.1 + 64 + 8 };
        self.window.set_outer_position(PhysicalPosition::new(bx, by.max(mon.y + 2)));
        if self.surface.resize(NonZeroU32::new(w as u32).unwrap(), NonZeroU32::new(h as u32).unwrap()).is_ok() {
            self.draw();
        }
        self.window.set_visible(true);
    }

    pub fn hide(&mut self) {
        self.window.set_visible(false);
    }

    fn draw(&mut self) {
        let (w, h) = self.size;
        let mut buf = vec![0u32; (w * h) as usize];
        let tail_y0 = if self.above { h - 7 } else { 0 };
        let body_y0 = if self.above { 0 } else { 7 };
        let body_y1 = if self.above { h - 7 } else { h };
        // 圆角矩形主体 + 边框
        for y in body_y0..body_y1 {
            for x in 0..w {
                let corner = |cx: i32, cy: i32| -> bool {
                    let (dx, dy) = (x - cx, y - cy);
                    dx * dx + dy * dy > 9 && (x < 4 || x >= w - 4) && (y < body_y0 + 4 || y >= body_y1 - 4)
                };
                let cut = corner(4, body_y0 + 4)
                    || corner(w - 5, body_y0 + 4)
                    || corner(4, body_y1 - 5)
                    || corner(w - 5, body_y1 - 5);
                if cut {
                    continue;
                }
                let edge = x < 2 || x >= w - 2 || y < body_y0 + 2 || y >= body_y1 - 2;
                buf[(y * w + x) as usize] = if edge { BORDER } else { PAPER };
            }
        }
        // 尾巴小三角
        let cx = w / 2;
        if self.above {
            for i in 0..6 {
                for x in (cx - 5 + i / 2)..=(cx + 5 - i / 2) {
                    let y = tail_y0 + i;
                    if x > 1 && x < w - 1 {
                        buf[(y * w + x) as usize] = if i >= 4 { PAPER } else { BORDER };
                    }
                }
            }
        } else {
            for i in 0..6 {
                for x in (cx - 5 + i / 2)..=(cx + 5 - i / 2) {
                    let y = tail_y0 + 5 - i;
                    if x > 1 && x < w - 1 {
                        buf[(y * w + x) as usize] = if i >= 4 { PAPER } else { BORDER };
                    }
                }
            }
        }
        // 文本（避开边框和尾巴区）
        let clip = (3, body_y0 + 3, w - 4, body_y1 - 4);
        TEXT.draw_clipped(&mut buf, w, h, clip, 12, body_y0 + 6, &self.lines, INK, false);
        if let Ok(mut b) = self.surface.buffer_mut() {
            for (dst, src) in b.iter_mut().zip(buf.iter()) {
                *dst = *src;
            }
            let _ = b.present();
        }
    }
}
