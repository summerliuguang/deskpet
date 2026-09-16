//! 右键菜单：自绘的半透明黑底白字圆角弹出菜单（替代 muda 原生菜单）。
//! 支持二级页面（互动/换装/表情/设置），悬停 select 高亮、按下 pressed 变色，
//! 鼠标移出菜单即关闭。切换开关类条目 (stay) 点击后菜单保持打开并刷新标签。

use crate::text::{base_px, ui, Rgb, TEXT};
use crate::{MonRect, SbSurface};
use std::{num::NonZeroU32, sync::Arc, time::{Duration, Instant}};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::ActiveEventLoop,
    window::Window,
};

const W: i32 = 188;
const ITEM_H: i32 = 28;
const SEP_H: i32 = 10;
const PAD: i32 = 8;
const RADIUS: i32 = 10;
// 以上为逻辑值，create 时按 UI 缩放生成实际值

const BG: u32 = 0xCD101014; // 半透明黑底
const BORDER: u32 = 0x38FFFFFF;
const TEXT_COLOR: Rgb = (242, 240, 236);
const HOVER_BG: u32 = 0x50E8A33D;
const PRESSED_BG: u32 = 0x90C07A28;

/// 菜单页面
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Root,
    Fun,
    Model,
    Costume,
    Expr,
    Set,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// 条目动作 id；None = 分隔线；"page:xxx" = 页面切换（内部处理）
    pub id: Option<String>,
    pub label: String,
    /// 点击后菜单保持打开（开关/选择类条目）
    pub stay: bool,
}

impl Entry {
    pub fn item(id: &str, label: impl Into<String>) -> Self {
        Self { id: Some(id.into()), label: label.into(), stay: false }
    }
    pub fn stay(id: &str, label: impl Into<String>) -> Self {
        Self { id: Some(id.into()), label: label.into(), stay: true }
    }
    pub fn sub(page: Page, label: impl Into<String>) -> Self {
        Self { id: Some(format!("page:{page:?}")), label: label.into(), stay: true }
    }
    pub fn back() -> Self {
        Self { id: Some("page:Root".into()), label: "◂ 返回".into(), stay: true }
    }
    pub fn sep() -> Self {
        Self { id: None, label: String::new(), stay: false }
    }
}

pub enum MenuOutcome {
    None,
    Close,
    /// 点击条目。stay=true 表示菜单保持打开（App 处理后应 set_entries 刷新标签）
    Action { id: String, stay: bool },
}

pub struct MenuWin {
    pub window: Arc<Window>,
    surface: SbSurface,
    /// 缩放后的实际尺寸/度量
    w: i32,
    item_h: i32,
    sep_h: i32,
    pad: i32,
    radius: i32,
    px: i32,
    pages: Vec<(Page, Vec<Entry>)>,
    page: Page,
    entries: Vec<Entry>,
    rows: Vec<(i32, i32, usize)>,
    total_h: i32,
    hover: Option<usize>,
    pressed: Option<usize>,
    cursor: (f64, f64),
    opened_at: Instant,
}

fn page_from_id(id: &str) -> Option<Page> {
    Some(match id.strip_prefix("page:")? {
        "Root" => Page::Root,
        "Fun" => Page::Fun,
        "Costume" => Page::Costume,
        "Expr" => Page::Expr,
        "Model" => Page::Model,
        "Set" => Page::Set,
        _ => return None,
    })
}

impl MenuWin {
    pub fn create(
        el: &ActiveEventLoop,
        pages: Vec<(Page, Vec<Entry>)>,
        start: Page,
    ) -> Option<Self> {
        let entries = pages
            .iter()
            .find(|(p, _)| *p == start)
            .map(|(_, e)| e.clone())
            .unwrap_or_default();
        let (w, item_h, sep_h, pad, radius, px) = (
            ui(W), ui(ITEM_H), ui(SEP_H), ui(PAD), ui(RADIUS), base_px(),
        );
        let total_h = Self::height_of_scaled(&entries, item_h, pad, sep_h);
        let mut attrs = Window::default_attributes()
            .with_inner_size(PhysicalSize::new(w as u32, total_h as u32))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_active(false)
            .with_title("deskpet-menu")
            .with_visible(false);
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let window = Arc::new(el.create_window(attrs).ok()?);
        let ctx = softbuffer::Context::new(window.clone()).ok()?;
        let mut surface = softbuffer::Surface::new(&ctx, window.clone()).ok()?;
        surface
            .resize(NonZeroU32::new(w as u32).unwrap(), NonZeroU32::new(total_h as u32).unwrap())
            .ok()?;
        Some(Self {
            window,
            surface,
            w,
            item_h,
            sep_h,
            pad,
            radius,
            px,
            pages,
            page: start,
            entries,
            rows: Vec::new(),
            total_h,
            hover: None,
            pressed: None,
            cursor: (0.0, 0.0),
            opened_at: Instant::now(),
        })
    }

    fn height_of_scaled(entries: &[Entry], item_h: i32, pad: i32, sep_h: i32) -> i32 {
        pad * 2
            + entries.iter().filter(|e| e.id.is_some()).count() as i32 * item_h
            + entries.iter().filter(|e| e.id.is_none()).count() as i32 * sep_h
    }

    /// 在光标处弹出；靠近屏幕下缘时改为向上一贴
    pub fn open(mut self, at: (i32, i32), mon: MonRect) -> Self {
        let x = at.0.clamp(mon.x + 4, (mon.x + mon.w - self.w - 4).max(mon.x + 4));
        let mut y = at.1;
        if y + self.total_h > mon.y + mon.h - 4 {
            y = at.1 - self.total_h;
        }
        y = y.max(mon.y + 4);
        self.window.set_outer_position(PhysicalPosition::new(x, y));
        self.window.set_visible(true);
        // 关键：弹出时光标可能悬在某个条目上但没有产生移动事件，
        // 用全局光标换算窗口本地坐标，立即初始化命中状态
        self.cursor = ((at.0 - x) as f64, (at.1 - y) as f64);
        self.hover = self.row_at(self.cursor.1).filter(|r| self.entries.get(*r).map(|e| e.id.is_some()).unwrap_or(false));
        self.opened_at = Instant::now();
        self.draw();
        self
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// 用新条目刷新当前页（stay 动作后由 App 调用，更新动态标签）
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        if let Some(page_entries) = self.pages.iter_mut().find(|(p, _)| *p == self.page) {
            page_entries.1 = entries.clone();
        }
        self.entries = entries;
        self.relayout();
    }

    fn relayout(&mut self) {
        self.total_h = Self::height_of_scaled(&self.entries, self.item_h, self.pad, self.sep_h);
        self.hover = None;
        self.pressed = None;
        let _ = self.window.request_inner_size(PhysicalSize::new(
            self.w as u32,
            self.total_h.max(1) as u32,
        ));
        let _ = self.surface.resize(
            NonZeroU32::new(self.w as u32).unwrap(),
            NonZeroU32::new(self.total_h.max(1) as u32).unwrap(),
        );
        self.draw();
    }

    fn row_at(&self, cy: f64) -> Option<usize> {
        self.rows
            .iter()
            .position(|(y0, y1, _)| cy >= *y0 as f64 && cy <= *y1 as f64)
    }

    pub fn handle(&mut self, event: &WindowEvent) -> MenuOutcome {
        match event {
            WindowEvent::RedrawRequested => {
                self.draw();
                MenuOutcome::None
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                let row = self.row_at(position.y);
                if row != self.hover {
                    self.hover = row;
                    self.window.request_redraw();
                }
                MenuOutcome::None
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // 按光标当前位置重算命中行，避免悬停状态过期
                self.pressed = self.row_at(self.cursor.1);
                if self.pressed.is_some() {
                    self.window.request_redraw();
                }
                MenuOutcome::None
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // MouseInput 不带坐标，用最近一次 CursorMoved 记录的位置
                let row = self.row_at(self.cursor.1);
                self.pressed = None;
                self.window.request_redraw();
                match row.and_then(|r| self.entries.get(r).and_then(|e| e.id.clone())) {
                    Some(id) => {
                        if let Some(page) = page_from_id(&id) {
                            self.switch_page(page);
                            self.draw();
                            MenuOutcome::None
                        } else {
                            let stay = self
                                .entries
                                .get(row.unwrap_or(usize::MAX))
                                .map(|e| e.stay)
                                .unwrap_or(false);
                            if stay {
                                self.hover = None;
                                MenuOutcome::Action { id, stay: true }
                            } else {
                                MenuOutcome::Action { id, stay: false }
                            }
                        }
                    }
                    None => MenuOutcome::None, // 点在分隔线/空白上，不关闭
                }
            }
            WindowEvent::CursorLeft { .. } => {
                // 刚弹出时的瞬时 MouseLeave 不可信
                if self.opened_at.elapsed() > Duration::from_millis(250) {
                    MenuOutcome::Close
                } else {
                    MenuOutcome::None
                }
            }
            WindowEvent::CloseRequested => MenuOutcome::Close,
            _ => MenuOutcome::None,
        }
    }

    fn switch_page(&mut self, page: Page) {
        self.page = page;
        if let Some((_, entries)) = self.pages.iter().find(|(p, _)| *p == page) {
            self.entries = entries.clone();
        }
        self.relayout();
    }

    pub fn draw(&mut self) {
        let total_h = self.total_h;
        let r = self.radius;
        // 圆角矩形内含判定（所有尺寸用物理宽 self.w，缩放下与缓冲一致）
        let inside = |px: i32, py: i32| -> bool {
            if px < 0 || py < 0 || px >= self.w || py >= total_h {
                return false;
            }
            let (lx, ly) = (px, py);
            let (rx, ry) = (self.w - 1 - lx, total_h - 1 - ly);
            for (cx, cy) in [(r, r), (rx, ry), (r, ry), (rx, r)] {
                if cx < r && cy < r {
                    let dx = cx - r;
                    let dy = cy - r;
                    if dx * dx + dy * dy > r * r {
                        return false;
                    }
                }
            }
            true
        };
        let mut buf = vec![0u32; (self.w * total_h) as usize];
        for y in 0..total_h {
            for x in 0..self.w {
                if inside(x, y) {
                    buf[(y * self.w + x) as usize] = BG;
                }
            }
        }
        // 1px 内描边（有外侧邻接的内部像素）
        let mut border = Vec::new();
        for y in 0..total_h {
            for x in 0..self.w {
                if !inside(x, y) {
                    continue;
                }
                let neigh_out = [-1i32, 1].iter().any(|d| {
                    !inside(x + d, y)
                }) || [-1i32, 1].iter().any(|d| !inside(x, y + d));
                if neigh_out {
                    border.push((x, y));
                }
            }
        }
        for (x, y) in border {
            buf[(y * self.w + x) as usize] = BORDER;
        }

        // 行布局 + select/pressed 底色 + 文本
        self.rows.clear();
        let lh = TEXT.line_height(self.px);
        let mut y = self.pad;
        for (idx, entry) in self.entries.iter().enumerate() {
            match entry.id {
                None => {
                    let ly = y + self.sep_h / 2;
                    for x in ui(12)..self.w - ui(12) {
                        if inside(x, ly) {
                            buf[(ly * self.w + x) as usize] = 0x24FFFFFF;
                        }
                    }
                    y += self.sep_h;
                }
                Some(_) => {
                    let (y0, y1) = (y, y + self.item_h);
                    let row = self.rows.len();
                    self.rows.push((y0, y1, idx));
                    let selected = self.hover == Some(row);
                    let pressed = self.pressed == Some(row);
                    if selected || pressed {
                        let bg = if pressed { PRESSED_BG } else { HOVER_BG };
                        let pa = (bg >> 24) & 0xFF;
                        for yy in y0.max(r)..(y1 - 1).min(total_h - r) {
                            for x in ui(4)..self.w - ui(4) {
                                let px = buf[(yy * self.w + x) as usize];
                                let mixch = |sc: u32, dc: u32| -> u32 {
                                    (sc * pa + dc * (255 - pa)) / 255
                                };
                                buf[(yy * self.w + x) as usize] = 0xFF000000
                                    | (mixch((bg >> 16) & 0xFF, (px >> 16) & 0xFF) << 16)
                                    | (mixch((bg >> 8) & 0xFF, (px >> 8) & 0xFF) << 8)
                                    | mixch(bg & 0xFF, px & 0xFF);
                            }
                        }
                    }
                    let color = if pressed { (255, 244, 224) } else { TEXT_COLOR };
                    let label = if entry.stay && !entry.label.starts_with("◂") {
                        format!("  {}", entry.label)
                    } else {
                        entry.label.clone()
                    };
                    TEXT.draw_clipped(
                        &mut buf, self.w, total_h,
                        (ui(6), y0, self.w - ui(6), y1),
                        self.px,
                        ui(12),
                        y0 + (self.item_h - lh) / 2,
                        &[label],
                        color,
                        false,
                    );
                    y += self.item_h;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 守护测试：draw 里缓冲行宽必须用物理宽 self.w（=ui(W)），
    /// 曾因误用逻辑常量 W(188) 在 UI 缩放 >100% 时越界写 panic。
    /// 扫描源码禁止按常量 W 计算索引/判边界/分配缓冲的写法
    /// （模式用 format! 构造，避免匹配到本测试自身源码）。
    #[test]
    fn draw_indexes_use_scaled_width_not_logical_constant() {
        let src = include_str!("menu.rs");
        let idx = format!("* {W} +");
        let bound = format!(">= {W}");
        let alloc = format!("({W} * total_h)");
        assert!(!src.contains(idx.as_str()), "缓冲索引禁止用逻辑常量 W，应统一 self.w");
        assert!(!src.contains(bound.as_str()), "边界判定禁止用逻辑常量 W，应统一 self.w");
        assert!(!src.contains(alloc.as_str()), "缓冲分配禁止用逻辑常量 W");
    }

    /// 缩放后的行高计算：条目与分隔线分开计数，分隔线不占条目高度
    #[test]
    fn height_counts_items_and_separators() {
        let entries = vec![Entry::item("a", "甲"), Entry::sep(), Entry::item("b", "乙")];
        let h = MenuWin::height_of_scaled(&entries, 28, 8, 10);
        assert_eq!(h, 8 * 2 + 28 * 2 + 10);
    }
}
