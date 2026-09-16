//! 气泡对话框：独立的无边框透明小窗，点击穿透（set_cursor_hittest(false)），
//! 显示在猫头顶，到时自动消失。纯软件渲染中文文本。

use crate::text::{base_px, ui, Rgb, TEXT};
use crate::SbSurface;
use std::{
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event_loop::ActiveEventLoop,
    window::{Window, WindowLevel},
};

const INK: Rgb = (45, 42, 38);
const PAPER: u32 = 0xFFFDF9EE;
const BORDER: u32 = 0xFF5A5040;
/// 光标条颜色（与文字同色系，直接预混 ARGB）
const INK_MIX: u32 = 0xFF2D2A26;

/// 取前 n 个字符（跨行），返回截断后的行集
fn truncate_lines(lines: &[String], n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut left = n;
    for line in lines {
        if left == 0 {
            break;
        }
        let len = line.chars().count();
        if len <= left {
            out.push(line.clone());
            left -= len;
        } else {
            out.push(line.chars().take(left).collect());
            left = 0;
        }
    }
    out
}

/// 打字机逐字显示状态
struct Typewriter {
    /// 全文总字符数（chars 计）
    full_len: usize,
    /// 已显示字符数
    shown: usize,
    /// 下次推进时刻
    next_at: Instant,
}

const TYPE_STEP_MS: u64 = 45;

pub struct BubbleWin {
    pub window: Arc<Window>,
    surface: SbSurface,
    lines: Vec<String>,
    px: i32,
    size: (i32, i32),
    above: bool,
    visible: bool,
    /// Some = 打字机逐字显示中（尺寸仍按全文布局，不跳动）
    typing: Option<Typewriter>,
}

impl BubbleWin {
    pub fn create(el: &ActiveEventLoop) -> Option<Self> {
        // 初始物理尺寸（show 时按内容重设；窗口与 surface 保持一致）
        let (iw, ih) = (ui(60), ui(40));
        #[allow(unused_mut)] // windows 下会追加 with_skip_taskbar
        let mut attrs = Window::default_attributes()
            .with_inner_size(PhysicalSize::new(iw as u32, ih as u32))
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
            .resize(NonZeroU32::new(iw as u32).unwrap(), NonZeroU32::new(ih as u32).unwrap())
            .ok()?;
        Some(Self {
            window,
            surface,
            lines: Vec::new(),
            size: (ui(60), ui(40)),
            above: true,
            px: base_px(),
            visible: false,
            typing: None,
        })
    }

    /// 贴着宠物定位（气泡中心对准宠物中心，上下贴边翻转）；show/reposition 共用
    fn place(
        &self,
        pet_pos: (i32, i32),
        pet_size: i32,
        mon: crate::MonRect,
        above: bool,
    ) -> (i32, i32) {
        let (w, h) = self.size;
        let bx = (pet_pos.0 + pet_size / 2 - w / 2)
            .clamp(mon.x + 4, (mon.x + mon.w - w - 4).max(mon.x + 4));
        let by = if above { pet_pos.1 - h - 8 } else { pet_pos.1 + pet_size + 8 };
        (bx, by.max(mon.y + 2))
    }

    /// 显示气泡。above=true 时尾巴朝下（气泡在猫头顶）。
    pub fn show(
        &mut self,
        text: &str,
        pet_pos: (i32, i32),
        pet_size: i32,
        mon: crate::MonRect,
        above: bool,
    ) {
        self.show_inner(text, pet_pos, pet_size, mon, above, true);
    }

    /// 即时显示全文（思考泡等动画文本不适合打字机）
    pub fn show_now(
        &mut self,
        text: &str,
        pet_pos: (i32, i32),
        pet_size: i32,
        mon: crate::MonRect,
        above: bool,
    ) {
        self.show_inner(text, pet_pos, pet_size, mon, above, false);
    }

    fn show_inner(
        &mut self,
        text: &str,
        pet_pos: (i32, i32),
        pet_size: i32,
        mon: crate::MonRect,
        above: bool,
        typewriter: bool,
    ) {
        let px = self.px;
        let layout = TEXT.layout(text, (200.0 * crate::ui_scale()) as i32, px);
        let w = (layout.width + ui(24)).clamp(ui(56), ui(240));
        let h = layout.height + ui(12) + ui(6);
        self.size = (w, h);
        self.lines = layout.lines;
        self.above = above;
        // 打字机：尺寸按全文定死，逐字填充；空白文本直接跳过
        let full_len: usize = self.lines.iter().map(|l| l.chars().count()).sum();
        self.typing = if typewriter && full_len > 1 {
            Some(Typewriter { full_len, shown: 1, next_at: Instant::now() + Duration::from_millis(TYPE_STEP_MS) })
        } else {
            None
        };
        let _ = self.window.request_inner_size(PhysicalSize::new(w as u32, h as u32));
        let (bx, by) = self.place(pet_pos, pet_size, mon, above);
        self.window.set_outer_position(PhysicalPosition::new(bx, by));
        if self.surface.resize(NonZeroU32::new(w as u32).unwrap(), NonZeroU32::new(h as u32).unwrap()).is_ok() {
            self.draw();
        }
        self.visible = true;
        self.window.set_visible(true);
    }

    /// 打字机推进一步；返回 true 表示仍在打字中
    pub fn advance_typing(&mut self) -> bool {
        let Some(t) = &mut self.typing else { return false };
        t.shown += 1;
        t.next_at = Instant::now() + Duration::from_millis(TYPE_STEP_MS);
        let done = t.shown >= t.full_len;
        if done {
            self.typing = None;
        }
        self.draw();
        !done
    }

    /// 打字中的下一次推进时刻
    pub fn next_tick_at(&self) -> Option<Instant> {
        self.typing.as_ref().map(|t| t.next_at)
    }

    /// 打字机剩余时长（显示到期时间要加上它）
    pub fn typing_remaining(&self) -> Duration {
        self.typing
            .as_ref()
            .map(|t| Duration::from_millis((t.full_len - t.shown) as u64 * TYPE_STEP_MS))
            .unwrap_or_default()
    }

    /// 宠物移动时保持气泡贴在宠物旁（不重绘，只挪位置）
    pub fn reposition(&mut self, pet_pos: (i32, i32), pet_size: i32, mon: crate::MonRect, above: bool) {
        let (bx, by) = self.place(pet_pos, pet_size, mon, above);
        self.window.set_outer_position(PhysicalPosition::new(bx, by));
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.typing = None;
        self.window.set_visible(false);
    }

    fn draw(&mut self) {
        let px = self.px;
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
        // 文本（避开边框和尾巴区）；打字中只画前 shown 个字符 + 光标条
        let clip = (3, body_y0 + 3, w - 4, body_y1 - 4);
        let shown_lines: Vec<String> = match &self.typing {
            Some(t) => truncate_lines(&self.lines, t.shown),
            None => self.lines.clone(),
        };
        TEXT.draw_clipped(&mut buf, w, h, clip, px, 12, body_y0 + 6, &shown_lines, INK, false);
        if self.typing.is_some() {
            // 光标条：紧跟最后可见字符
            let last = shown_lines.last().map(|s| s.as_str()).unwrap_or("");
            let cx = 12 + TEXT.text_width(last, px);
            let cy = body_y0 + 6 + (shown_lines.len().saturating_sub(1)) as i32 * TEXT.line_height(px);
            for dy in 0..TEXT.line_height(px) - 2 {
                for dx in 0..ui(2).max(1) {
                    let (x, y) = (cx + dx, cy + dy + 1);
                    if x > 2 && x < w - 3 && y > body_y0 + 2 && y < body_y1 - 3 {
                        buf[(y * w + x) as usize] = INK_MIX;
                    }
                }
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
