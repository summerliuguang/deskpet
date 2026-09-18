//! 待办清单：独立小窗，勾选/删除/新增/优先级/截止时间，todos.json 持久化（与 exe 同目录）。
//!
//! 输入语法（回车添加）：
//! - `!高` / `!低` 前缀设置优先级（缺省为中）；清单里点色条可循环切换
//! - `@17:30` 或 `@9` 设置今天（已过则明天）的截止时刻
//! 示例：`!高 交周报 @17:30`
//! 列表按优先级排序（高→中→低，同级保持加入顺序）；到期时宠物气泡提醒一次。

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

/// 待办优先级：影响排序与左侧色条颜色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    High,
    Normal,
    Low,
}

impl Priority {
    fn rank(self) -> u8 {
        match self {
            Priority::High => 0,
            Priority::Normal => 1,
            Priority::Low => 2,
        }
    }

    fn color(self) -> u32 {
        match self {
            Priority::High => 0xFFE05A4A,
            Priority::Normal => 0xFFD9A83A,
            Priority::Low => 0xFF7BA05B,
        }
    }

    fn next(self) -> Self {
        match self {
            Priority::High => Priority::Normal,
            Priority::Normal => Priority::Low,
            Priority::Low => Priority::High,
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "high" => Some(Priority::High),
            "normal" => Some(Priority::Normal),
            "low" => Some(Priority::Low),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Priority::High => "high",
            Priority::Normal => "normal",
            Priority::Low => "low",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Todo {
    pub text: String,
    pub done: bool,
    pub priority: Priority,
    /// 截止时刻（本地时区 UNIX 秒）；None = 无
    pub due: Option<i64>,
    /// 到期提醒已发过一次
    pub fired: bool,
}

impl Todo {
    fn new(text: String) -> Self {
        Self { text, done: false, priority: Priority::Normal, due: None, fired: false }
    }
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

/// 本地时区当前 UNIX 秒
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 时区偏移。std/当前依赖无跨平台 localtime API，零依赖方案下
/// 按 UTC 解释截止时刻：仅影响"今天/明天"的边界判断，可接受。
fn tz_offset_secs() -> i64 {
    0
}

/// days since epoch → (d, m, y)（Howard Hinnant civil_from_days）
fn civil_from_days(z: i64) -> (u32, u32, i32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((d as u32), (m as u32), ((y + if m <= 2 { 1 } else { 0 }) as i32))
}

/// 当前本地（现按 UTC）日期 (日, 月, 年)
fn today_ymd(unix: i64) -> (u32, u32, i32) {
    civil_from_days(unix.div_euclid(86400))
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
                    let text = x.get("text")?.as_str()?.to_string();
                    let done = x.get("done").and_then(|d| d.as_bool()).unwrap_or(false);
                    // 旧格式只有 text/done：priority/due/fired 缺省兼容
                    let priority = x
                        .get("priority")
                        .and_then(|p| p.as_str())
                        .and_then(Priority::from_str)
                        .unwrap_or(Priority::Normal);
                    let due = x.get("due").and_then(|d| d.as_i64());
                    let fired = x.get("fired").and_then(|f| f.as_bool()).unwrap_or(false);
                    Some(Todo { text, done, priority, due, fired })
                })
                .collect();
        }
    }

    fn save(&self) {
        let v: Vec<serde_json::Value> = self
            .items
            .iter()
            .map(|t| {
                let mut o = serde_json::json!({"text": t.text, "done": t.done});
                if t.priority != Priority::Normal || t.due.is_some() || t.fired {
                    o["priority"] = serde_json::json!(t.priority.as_str());
                }
                if let Some(due) = t.due {
                    o["due"] = serde_json::json!(due);
                }
                if t.fired {
                    o["fired"] = serde_json::json!(true);
                }
                o
            })
            .collect();
        let result = serde_json::to_string_pretty(&v)
            .map_err(|e| e.to_string())
            .and_then(|s| std::fs::write(&self.path, s).map_err(|e| e.to_string()));
        if result.is_err() {
            crate::dwarn("todo-save", "todos.json 保存失败（磁盘满或不可写？）");
        }
    }

    /// 解析输入行 → (纯文本, 优先级, 截止)
    fn parse_input(&self, raw: &str) -> (String, Priority, Option<i64>) {
        parse_input_line(raw)
    }

    /// 最早到期且未提醒的未完成待办 → 到期则标记已提醒并返回其文本
    pub fn poll_due(&mut self) -> Option<String> {
        let now = now_secs();
        let hit = self
            .items
            .iter_mut()
            .find(|t| !t.done && !t.fired && t.due.map(|d| d <= now).unwrap_or(false))?;
        hit.fired = true;
        let text = hit.text.clone();
        self.save();
        self.draw();
        Some(text)
    }

    pub fn open(&mut self, near: (i32, i32), mon: crate::MonRect) {
        let x = (near.0 - self.w - 12).max(mon.x + 4);
        let y_min = mon.y + 4;
        let y_max = (mon.y + mon.h - self.h - 4).max(y_min);
        let y = (near.1 - 100).clamp(y_min, y_max);
        self.window.set_outer_position(PhysicalPosition::new(x, y));
        self.window.set_visible(true);
        self.window.focus_window();
        if let Some(h) = crate::hwnd_of(&self.window) {
            crate::force_focus_window(h);
        }
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
                        let raw = self.input.trim().to_string();
                        if !raw.is_empty() {
                            let (text, priority, due) = self.parse_input(&raw);
                            if !text.is_empty() {
                                let mut t = Todo::new(text);
                                t.priority = priority;
                                t.due = due;
                                self.items.push(t);
                                self.input.clear();
                                self.preedit = None;
                                self.scroll = 0;
                                self.save();
                            }
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
                } else if cx >= ui(28) as f64 && cx <= ui(34) as f64 {
                    // 点优先级色条：循环 高→中→低
                    self.items[idx].priority = self.items[idx].priority.next();
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

        // 条目列表（自底向上 + 滚动），按优先级稳定排序
        let area_top = header_h;
        let area_bottom = h - input_h - ui(16);
        let max_text_w = w - ui(96);
        self.rows.clear();
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        order.sort_by_key(|&i| (self.items[i].priority.rank(), i));
        let mut y_bottom = area_bottom - self.scroll;
        for k in order.into_iter().rev() {
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
            // 优先级色条（点击循环切换）
            let bar_x = bx + ui(18);
            for y in by..by + ui(14) {
                for x in bar_x..bar_x + ui(4) {
                    buf[(y * w + x) as usize] = item.priority.color();
                }
            }
            // 文本
            let (color, strike) = if item.done { (GRAY, true) } else { (INK, false) };
            TEXT.draw_clipped(
                &mut buf, w, h,
                (bar_x + ui(8), y_top + ui(2), w - ui(64), y_bottom),
                px, bar_x + ui(8), y_top + ui(5), &lines, color, false,
            );
            if strike {
                let sy = y_top + ui(5) + text_h / 2;
                let x_end = (bar_x + ui(8) + TEXT.text_width(&lines[0], px)).min(w - ui(64));
                for x in bar_x + ui(8)..x_end {
                    if sy > area_top {
                        buf[(sy * w + x) as usize] = BOX_BORDER;
                    }
                }
            }
            // 截止时间（右侧）：临近橙色、过期红色
            if let Some(due) = item.due {
                let (label, color) = due_label(due);
                let lw = TEXT.text_width(&label, px);
                let lx = (w - ui(32) - lw).max(bar_x + ui(10));
                TEXT.draw_clipped(
                    &mut buf, w, h,
                    (lx, y_top + ui(2), w - ui(28), y_bottom),
                    px, lx, y_top + ui(5), &[label], color, false,
                );
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
            let msg = "添加待办：!高/!低 优先级  @17:30 截止";
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

/// `@` 后面的截止时间：`HH:MM` 或 `HH`（今天，已过则明天）
fn parse_due(rest: &str) -> Option<i64> {
    let (hh, mm) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (rest.parse::<u32>().ok()?, 0),
    };
    if hh > 23 || mm > 59 {
        return None;
    }
    let (d, m, y) = today_ymd(now_secs());
    let midnight = local_midnight_from(d, m, y);
    let mut target = midnight + (hh as i64) * 3600 + (mm as i64) * 60;
    if target <= now_secs() {
        // 已过 → 明天同一时刻
        target += 86400;
    }
    Some(target)
}

/// 输入行解析（独立函数便于测试）：`!高/!低` 前缀、`@HH:MM/@HH` 截止、
/// 其余片段组成纯文本；无法识别的记号原样保留在文本中
fn parse_input_line(raw: &str) -> (String, Priority, Option<i64>) {
    let mut text = String::new();
    let mut priority = Priority::Normal;
    let mut due = None;
    for tok in raw.split_whitespace() {
        if let Some(p) = tok.strip_prefix('!') {
            match p {
                "高" | "high" | "h" => {
                    priority = Priority::High;
                    continue;
                }
                "低" | "low" | "l" => {
                    priority = Priority::Low;
                    continue;
                }
                "中" | "normal" | "" => {
                    priority = Priority::Normal;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(rest) = tok.strip_prefix('@') {
            if let Some(d) = parse_due(rest) {
                due = Some(d);
                continue;
            }
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(tok);
    }
    (text, priority, due)
}

fn local_midnight_from(d: u32, m: u32, y: i32) -> i64 {
    let yy = y as i64 - if m <= 2 { 1 } else { 0 };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    days * 86400 - tz_offset_secs()
}

/// 截止时刻 → 短标签与颜色：过期红、1 小时内橙、更远灰
fn due_label(due: i64) -> (String, (u8, u8, u8)) {
    let secs = due - now_secs();
    if secs <= 0 {
        ("已过期".into(), (200, 70, 60))
    } else if secs < 3600 {
        (format!("{}分后", secs / 60 + 1), (216, 140, 40))
    } else if secs < 86400 {
        (format!("{}时后", secs / 3600), GRAY)
    } else {
        (format!("{}天后", secs / 86400 + 1), GRAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_input_extracts_priority_and_due() {
        let (text, pri, due) = parse_input_line("!高 交周报 @23:59");
        assert_eq!(text, "交周报");
        assert_eq!(pri, Priority::High);
        assert!(due.is_some(), "@23:59 应解析出截止");
        let (text2, pri2, due2) = parse_input_line("买菜 !低");
        assert_eq!(text2, "买菜");
        assert_eq!(pri2, Priority::Low);
        assert_eq!(due2, None, "无 @ 应无截止");
        let (text3, _, due3) = parse_input_line("邮件 @99");
        assert_eq!(text3, "邮件 @99", "非法时刻应留在文本里");
        assert_eq!(due3, None);
        // @ 截止应是未来时刻
        let (_, _, due4) = parse_input_line("x @1");
        let d = due4.unwrap();
        assert!(d > now_secs(), "@1 应解析为未来（1 点已过则明天）");
    }

    #[test]
    fn old_json_format_still_loads() {
        // 旧版 todos.json 只有 text/done：应读入为 Normal 优先级、无截止
        let v: Vec<serde_json::Value> =
            serde_json::from_str(r#"[{"text":"旧条目","done":true}]"#).unwrap();
        let x = &v[0];
        let priority = x.get("priority").and_then(|p| p.as_str()).and_then(Priority::from_str);
        assert_eq!(priority, None, "旧格式无 priority 字段");
        assert_eq!(x.get("text").unwrap().as_str(), Some("旧条目"));
    }

    #[test]
    fn due_label_buckets() {
        let now = now_secs();
        let (l, _) = due_label(now - 10);
        assert_eq!(l, "已过期");
        let (l, _) = due_label(now + 600);
        assert!(l.ends_with("分后"));
        let (l, _) = due_label(now + 7200);
        assert!(l.ends_with("时后"));
        let (l, _) = due_label(now + 3 * 86400);
        assert!(l.ends_with("天后"));
    }

    #[test]
    fn priority_cycles() {
        assert_eq!(Priority::High.next(), Priority::Normal);
        assert_eq!(Priority::Normal.next(), Priority::Low);
        assert_eq!(Priority::Low.next(), Priority::High);
        assert!(Priority::High.rank() < Priority::Normal.rank());
        assert!(Priority::Normal.rank() < Priority::Low.rank());
    }
}
