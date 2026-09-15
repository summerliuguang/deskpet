//! 待办清单：独立小窗，增删勾选，todos.json 持久化（与 exe 同目录）。
//! 尺寸与文字按 UI 缩放渲染。

use crate::text::{base_px, ui, TEXT};
use crate::SbSurface;
use std::{num::NonZeroU32, path::PathBuf, sync::Arc};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, MouseButton, WindowEvent},
    event_loop::ActiveEventLoop,
    window::Window,
};

const W: i32 = 264;
const H: i32 = 340;
const HEADER_H: i32 = 26;
const INPUT_H: i32 = 36;
const ROW_H: i32 = 26;

const BG: u32 = 0xFFF6F2E9;
const HEADER_BG: u32 = 0xFF3A3644;
const INK: (u8, u8, u8) = (45, 42, 38);
const GRAY: (u8, u8, u8) = (150, 144, 132);
const WHITE: (u8, u8, u8) = (255, 255, 255);
const BOX_BORDER: u32 = 0xFF8A8478;
const BOX_DONE: u32 = 0xFF7BA05B;

#[derive(Debug, Clone)]
pub struct Todo {
    pub text: String,
    pub done: bool,
}

pub enum TodoAction {
    None,
    Close,
}

pub struct TodoWin {
    pub window: Arc<Window>,
    surface: SbSurface,
    items: Vec<Todo>,
    input: String,
    preedit: Option<String>,
    scroll: i32,
    cursor: (f64, f64),
    path: PathBuf,
    /// 每行 (y_top, y_bottom, 条目下标)，供点击命中
    rows: Vec<(i32, i32, usize)>,
    /// 实际渲染尺寸（按 UI 缩放）
    w: i32,
    h: i32,
    row_h: i32,
    px: i32,
}

impl TodoWin {
    pub fn create(el: &ActiveEventLoop) -> Option<Self> {
        let (w, h) = (ui(W), ui(H));
        let attrs = Window::default_attributes()
            .with_inner_size(PhysicalSize::new(w as u32, h as u32))
            .with_decorations(false)
            .with_resizable(false)
            .with_title("deskpet-todo")
            .with_visible(false);
        let window = Arc::new(el.create_window(attrs).ok()?);
        window.set_ime_allowed(true);
        let ctx = softbuffer::Context::new(window.clone()).ok()?;
        let mut surface = softbuffer::Surface::new(&ctx, window.clone()).ok()?;
        surface.resize(NonZeroU32::new(w as u32).unwrap(), NonZeroU32::new(h as u32).unwrap()).ok()?;
        let path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("todos.json")))
            .unwrap_or_else(|| PathBuf::from("todos.json"));
        let mut this = Self {
            window,
            surface,
            items: Vec::new(),
            input: String::new(),
            preedit: None,
            scroll: 0,
            cursor: (0.0, 0.0),
            path,
            rows: Vec::new(),
            w,
            h,
            row_h: ui(ROW_H),
            px: base_px(),
        };
        this.load();
        Some(this)
    }

    fn load(&mut self) {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return;
        };
        if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
            self.items = v
                .into_iter()
                .filter_map(|x| {
                    Some(Todo {
                        text: x.get("text")?.as_str()?.to_string(),
                        done: x.get("done").and_then(|d| d.as_bool()).unwrap_or(false),
                    })
                })
                .collect();
        }
    }

    fn save(&self) {
        let v: Vec<serde_json::Value> = self
            .items
            .iter()
            .map(|t| serde_json::json!({"text": t.text, "done": t.done}))
            .collect();
        let _ = serde_json::to_string_pretty(&v).map(|s| std::fs::write(&self.path, s));
    }

    pub fn open(&mut self, near: (i32, i32), mon: crate::MonRect) {
        let x = (near.0 - self.w - 12).max(mon.x + 4);
        let y_min = mon.y + 4;
        let y_max = (mon.y + mon.h - self.h - 4).max(y_min);
        let y = (near.1 - 100).clamp(y_min, y_max);
        self.window.set_outer_position(PhysicalPosition::new(x, y));
        self.window.set_visible(true);
        self.window.focus_window();
        self.draw();
    }

    pub fn handle(&mut self, event: &WindowEvent) -> TodoAction {
        match event {
            WindowEvent::RedrawRequested => {
                self.draw();
                TodoAction::None
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                TodoAction::None
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.click();
                TodoAction::None
            }
            WindowEvent::MouseWheel { delta, .. } => {
                use winit::event::MouseScrollDelta;
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (*y * 40.0) as i32,
                    MouseScrollDelta::PixelDelta(p) => p.y as i32,
                };
                self.scroll = (self.scroll - dy).max(0);
                self.draw();
                TodoAction::None
            }
            WindowEvent::KeyboardInput { event, .. } => {
                use winit::keyboard::{Key, NamedKey};
                if event.state != ElementState::Pressed {
                    return TodoAction::None;
                }
                match event.logical_key {
                    Key::Named(NamedKey::Enter) => {
                        let text = self.input.trim().to_string();
                        if !text.is_empty() {
                            self.input.clear();
                            self.preedit = None;
                            self.items.push(Todo { text, done: false });
                            self.scroll = 0;
                            self.save();
                        }
                        self.draw();
                        TodoAction::None
                    }
                    Key::Named(NamedKey::Backspace) => {
                        self.input.pop();
                        self.draw();
                        TodoAction::None
                    }
                    Key::Named(NamedKey::Escape) => TodoAction::Close,
                    _ => TodoAction::None,
                }
            }
            WindowEvent::Ime(Ime::Commit(s)) => {
                self.input.push_str(s);
                self.preedit = None;
                self.draw();
                TodoAction::None
            }
            WindowEvent::Ime(Ime::Preedit(s, _)) => {
                self.preedit = if s.is_empty() { None } else { Some(s.clone()) };
                self.draw();
                TodoAction::None
            }
            WindowEvent::CloseRequested => {
                self.save();
                TodoAction::Close
            }
            _ => TodoAction::None,
        }
    }

    fn click(&mut self) {
        let (cx, cy) = self.cursor;
        for (y0, y1, idx) in self.rows.clone() {
            if cy >= y0 as f64 && cy <= y1 as f64 && idx < self.items.len() {
                if cx >= ui(8) as f64 && cx <= ui(26) as f64 {
                    self.items[idx].done = !self.items[idx].done;
                    self.save();
                } else if cx >= (self.w - ui(30)) as f64 {
                    self.items.remove(idx);
                    self.save();
                }
                self.draw();
                return;
            }
        }
    }

    pub fn draw(&mut self) {
        let (w, h) = (self.w, self.h);
        let px = self.px;
        let lh = TEXT.line_height(px);
        let header_h = ui(HEADER_H);
        let input_h = ui(INPUT_H);
        let _hint_top = h - input_h - ui(16);
        let mut buf = vec![BG; (w * h) as usize];
        // 标题栏
        for y in 0..header_h {
            for x in 0..w {
                buf[(y * w + x) as usize] = HEADER_BG;
            }
        }
        TEXT.draw(&mut buf, w, h, px, ui(8), 3, &["待办清单".to_string()], WHITE, false);

        // 输入框
        for y in h - input_h..h {
            for x in ui(2)..w - ui(2) {
                let edge = y == h - input_h || y == h - 1 || x == ui(2) || x == w - ui(3);
                buf[(y * w + x) as usize] = if edge { BOX_BORDER } else { 0xFFFFFFFF };
            }
        }
        let mut shown = self.input.clone();
        if let Some(p) = &self.preedit {
            shown.push_str(p);
        }
        while TEXT.text_width(&shown, px) > w - ui(24) && !shown.is_empty() {
            let mut it = shown.chars();
            it.next();
            shown = it.as_str().to_string();
        }
        TEXT.draw_clipped(
            &mut buf, w, h,
            (ui(4), h - input_h + ui(2), w - ui(6), h - ui(2)),
            px, ui(8), h - input_h + ui(4), &[shown], INK, false,
        );
        let caret_x = (ui(8) + TEXT.text_width(&self.input, px)).min(w - ui(12));
        for y in h - input_h + ui(6)..h - ui(6) {
            buf[(y * w + caret_x) as usize] = 0xFF3A3644;
        }

        // 条目列表（自底向上 + 滚动）
        let area_top = header_h;
        let area_bottom = h - input_h - ui(16);
        let max_text_w = w - ui(68);
        self.rows.clear();
        let mut y_bottom = area_bottom - self.scroll;
        for k in (0..self.items.len()).rev() {
            let item = &self.items[k];
            let lines = TEXT.layout(&item.text, max_text_w, px).lines;
            let text_h = lines.len() as i32 * lh;
            let bh = text_h.max(self.row_h - ui(8)) + ui(10);
            let y_top = y_bottom - bh;
            if y_top < area_top {
                break;
            }
            self.rows.push((y_top, y_bottom, k));
            // 勾选框
            let bx = ui(10);
            let by = y_top + (bh - ui(14)) / 2;
            for y in by..by + ui(14) {
                for x in bx..bx + ui(14) {
                    let edge = y == by || y == by + ui(13) || x == bx || x == bx + ui(13);
                    buf[(y * w + x) as usize] = if item.done && !edge {
                        BOX_DONE
                    } else {
                        BOX_BORDER
                    };
                }
            }
            if item.done {
                TEXT.draw_clipped(
                    &mut buf, w, h,
                    (bx, by, bx + ui(14), by + ui(14)),
                    px, bx + ui(2), by, &["y".to_string()], WHITE, false,
                );
            }
            // 文本
            let (color, strike) = if item.done { (GRAY, true) } else { (INK, false) };
            TEXT.draw_clipped(
                &mut buf, w, h,
                (bx + ui(22), y_top + ui(2), w - bx - ui(8), y_bottom),
                px, bx + ui(22), y_top + ui(5), &lines, color, false,
            );
            if strike {
                let sy = y_top + ui(5) + text_h / 2;
                for x in bx + ui(22)..(bx + ui(22) + TEXT.text_width(&lines[0], px)).min(w - bx - ui(8)) {
                    if sy > area_top {
                        buf[(sy * w + x) as usize] = BOX_BORDER;
                    }
                }
            }
            // 删除 ×
            TEXT.draw_clipped(
                &mut buf, w, h,
                (w - ui(30), y_top, w, y_bottom),
                px, w - ui(28), y_top + ui(5), &["x".to_string()], GRAY, false,
            );
            y_bottom = y_top - ui(2);
            if y_bottom < area_top {
                break;
            }
        }
        if self.items.is_empty() {
            let msg = "还没有待办，在下面输入回车添加";
            TEXT.draw(
                &mut buf, w, h,
                px,
                (w - TEXT.text_width(msg, px)) / 2,
                (h - input_h + header_h) / 2,
                &[msg.to_string()], GRAY, false,
            );
        }

        if let Ok(mut b) = self.surface.buffer_mut() {
            for (dst, src) in b.iter_mut().zip(buf.iter()) {
                *dst = *src;
            }
            let _ = b.present();
        }
    }
}
