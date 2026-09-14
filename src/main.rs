//! 纯 Rust 桌宠：winit + softbuffer 软件渲染，不创建任何 GPU 上下文。
//!
//! 低占用纪律：
//! - 只在动画帧/气泡/窗口内容变化时重绘；睡觉和全屏遮挡时事件循环完全静止
//! - 键盘钩子回调只发一个事件；手柄 10Hz 轮询；TTS/扔窗口在独立线程
//! - 目光跟随只在动画 tick 里读一次 GetCursorPos
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, Instant},
};

use deskpet::{
    ai::{build_history, ChatMsg, Client, Role},
    bubble::BubbleWin,
    config::{Config, Settings},
    inputbox::{InputAction, InputBox},
    menu::{Entry, MenuOutcome, MenuWin, Page},
    model::{PetModel, PetState, Pose, PixelCat},
    todo::{TodoAction, TodoWin},
    tts, MonRect, PetEvent, PetEventProxy, SbSurface,
};
#[cfg(windows)]
use deskpet::{input, sprites};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, Ime, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId, WindowLevel},
};

const PET_SIZE: i32 = 64;
const WALK_STEP: i32 = 4;
const CLIMB_STEP: i32 = 2;
const WALK_FRAME_MS: u64 = 180;
const IDLE_FRAME_MS: u64 = 600;
const DRAG_FRAME_MS: u64 = 140;
const THROWN_FRAME_MS: u64 = 16;
const DRAG_START_MS: u64 = 260;
const CLICK_MAX_MS: u64 = 500;
/// 甩出判定：拖拽释放速度阈值（像素/毫秒）
const FLING_SPEED_MIN: f32 = 0.4;
/// 重力（像素/毫秒²）
const GRAVITY: f32 = 0.0009;

const WHISPERS: &[&str] = &[
    "铲屎的，摸摸头？",
    "小鱼干还有吗？",
    "屏幕好亮，眯一会儿……",
    "陪我玩一会儿嘛～",
    "今天的代码写完了吗？",
    "（盯着光标看）那是老鼠吗？",
];

const PAD_REACTIONS: &[&str] = &["手柄真好玩！", "摇杆搓得不错嘛！", "按键的声音好清脆！"];

#[derive(Debug, Clone, Copy)]
struct PressInfo {
    start: Instant,
    moved: bool,
}

struct App {
    proxy: PetEventProxy,
    window: Option<Arc<Window>>,
    surface: Option<SbSurface>,
    bubble: Option<BubbleWin>,
    input: Option<InputBox>,
    menu: Option<MenuWin>,
    todo: Option<TodoWin>,
    #[cfg(windows)]
    tray: Option<tray_icon::TrayIcon>,
    client: Client,
    cfg: Config,
    settings: Settings,
    model: Box<dyn PetModel>,

    state: PetState,
    tick: u32,
    state_len: u32,
    idle_cycles: u32,
    dir: i32,
    pos: (i32, i32),
    /// 当前所在显示器（Wander/爬墙/弹跳边界）
    mon: MonRect,
    mons: Vec<MonRect>,
    hidden: bool,

    frame_at: Option<Instant>,
    bubble_until: Option<Instant>,
    reaction_end: Option<Instant>,
    whisper_at: Option<Instant>,
    typing_until: Option<Instant>,
    hang_until: Option<Instant>,
    press: Option<PressInfo>,
    press_cursor: (f64, f64),
    drag_origin: (i32, i32),
    grab: (f64, f64),
    drag_track: Vec<(Instant, f64, f64)>,
    thrown_vel: (f32, f32),
    climb_wall: i32,
    climb_vertical: i32,
    last_click: Option<(Instant, (f64, f64))>,
    cursor: (f64, f64),
    rng: u64,

    /// AI 对话历史（气泡模式无历史 UI，但上下文保留在内存）
    chat_history: Vec<ChatMsg>,
    chat_pending: bool,
    think_next: Option<Instant>,
    think_frame: u32,
    /// 趴窗状态：(窗口句柄地址, 相对窗口左缘的偏移, 剩余 tick)
    perch: Option<(isize, i32, u32)>,
    drink_at: Option<Instant>,
    sit_at: Option<Instant>,
}

impl App {
    fn new(cfg: Config, settings: Settings, proxy: PetEventProxy) -> Self {
        Self {
            proxy,
            window: None,
            surface: None,
            bubble: None,
            input: None,
            menu: None,
            todo: None,
            #[cfg(windows)]
            tray: None,
            client: Client::new(),
            cfg,
            settings,
            model: Box::new(PixelCat::new()),
            state: PetState::Idle,
            tick: 0,
            state_len: 60,
            idle_cycles: 0,
            dir: 1,
            pos: (0, 0),
            mon: MonRect { x: 0, y: 0, w: 1280, h: 720 },
            mons: vec![MonRect { x: 0, y: 0, w: 1280, h: 720 }],
            hidden: false,
            frame_at: None,
            bubble_until: None,
            reaction_end: None,
            whisper_at: None,
            typing_until: None,
            hang_until: None,
            press: None,
            press_cursor: (0.0, 0.0),
            drag_origin: (0, 0),
            grab: (0.0, 0.0),
            drag_track: Vec::new(),
            thrown_vel: (0.0, 0.0),
            climb_wall: 0,
            climb_vertical: 0,
            last_click: None,
            cursor: (0.0, 0.0),
            rng: 0x9E3779B97F4A7C15,
            chat_history: Vec::new(),
            chat_pending: false,
            think_next: None,
            think_frame: 0,
            perch: None,
            drink_at: None,
            sit_at: None,
        }
    }

    fn rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn rand_range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.rand() % (hi - lo)
    }

    fn mon_bottom(&self) -> i32 {
        self.mon.y + self.mon.h - PET_SIZE
    }

    // ---------- 状态机 ----------

    fn enter_idle(&mut self) {
        if self.state != PetState::Idle {
            self.idle_cycles = 0;
        }
        self.state = PetState::Idle;
        self.tick = 0;
        self.state_len = self.rand_range(30, 90) as u32;
    }

    fn enter_walk(&mut self) {
        self.state = PetState::Walk;
        self.tick = 0;
        self.state_len = self.rand_range(20, 60) as u32;
        self.dir = if self.rand() % 2 == 0 { 1 } else { -1 };
    }

    fn enter_pose(&mut self, state: PetState, ticks: u32, msg: Option<&str>) {
        self.state = state;
        self.tick = 0;
        self.state_len = ticks;
        self.frame_at = None;
        if let Some(m) = msg {
            self.bubble_show(m);
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn enter_climb(&mut self, wall: i32) {
        self.state = PetState::Climb;
        self.tick = 0;
        self.climb_wall = wall;
        self.climb_vertical = -1;
        self.hang_until = None;
        self.bubble_show("爬墙咯！");
    }

    fn enter_sleep_by_bubble(&mut self, msg: &str) {
        self.bubble_show(msg);
        self.state = PetState::Sleep;
        self.tick = 0;
        self.frame_at = None;
        self.reaction_end = None;
        self.hang_until = None;
    }

    fn wake_with(&mut self, msg: &str) {
        self.enter_idle();
        self.reaction_end = None;
        self.hang_until = None;
        self.frame_at = None;
        self.bubble_show(msg);
    }

    fn gaze(&self) -> (i32, i32) {
        if !self.settings.gaze_follow
            || (self.state != PetState::Idle && self.state != PetState::Walk)
        {
            return (0, 0);
        }
        #[cfg(windows)]
        {
            if let Some((mx, my)) = cursor_pos() {
                let (cx, cy) = (self.pos.0 + PET_SIZE / 2, self.pos.1 + PET_SIZE / 2);
                let (dx, dy) = (mx - cx, my - cy);
                let gx = if dx > 28 { 1 } else if dx < -28 { -1 } else { 0 };
                let gy = if dy > 28 { 1 } else if dy < -28 { -1 } else { 0 };
                return (gx, gy);
            }
        }
        (0, 0)
    }

    fn advance(&mut self, window: &Window) {
        // 跟随鼠标模式：朝光标水平位置走，靠近后坐下看
        if self.settings.follow_mouse
            && matches!(self.state, PetState::Idle | PetState::Walk)
        {
            #[cfg(windows)]
            if let Some((mx, _)) = cursor_pos() {
                let target = mx - PET_SIZE / 2;
                let dx = target - self.pos.0;
                if dx.abs() > 32 {
                    if self.state == PetState::Idle {
                        self.enter_walk();
                    }
                    self.dir = dx.signum();
                } else if self.state == PetState::Walk {
                    self.enter_idle();
                }
            }
        }
        match self.state {
            PetState::Idle => {
                self.tick += 1;
                if self.tick >= self.state_len {
                    if self.idle_cycles >= 2 {
                        self.state = PetState::Sleep;
                        self.tick = 0;
                    } else {
                        self.idle_cycles += 1;
                        self.enter_walk();
                    }
                }
            }
            PetState::Walk => {
                self.tick += 1;
                self.pos.0 += self.dir * WALK_STEP;
                if self.pos.0 < self.mon.x {
                    self.pos.0 = self.mon.x;
                    self.dir = 1;
                    // 撞左墙：概率爬墙
                    if self.mon.h >= 240 && self.rand() % 10 < 4 {
                        self.pos.0 += WALK_STEP;
                        self.enter_climb(-1);
                    }
                } else if self.pos.0 > self.mon.x + self.mon.w - PET_SIZE {
                    self.pos.0 = self.mon.x + self.mon.w - PET_SIZE;
                    self.dir = -1;
                    if self.mon.h >= 240 && self.rand() % 10 < 4 {
                        self.pos.0 -= WALK_STEP;
                        self.enter_climb(1);
                    }
                }
                window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
                if self.state == PetState::Walk && self.tick >= self.state_len {
                    // 随机休息姿势：坐 / 伸懒腰 / 舔毛，小概率去趴窗
                    let roll = self.rand() % 100;
                    if roll <= 9 && self.mon.h >= 240 {
                        self.perch_on_window();
                    } else if roll <= 34 {
                        let t = self.rand_range(6, 14) as u32;
                        self.enter_pose(PetState::Sitting, t, None);
                    } else if roll <= 49 {
                        self.enter_pose(PetState::Stretch, 4, Some("伸个懒腰～"));
                    } else if roll <= 69 {
                        let t = self.rand_range(8, 16) as u32;
                        self.enter_pose(PetState::Groom, t, None);
                    } else {
                        self.enter_idle();
                    }
                }
            }
            PetState::Sitting | PetState::Stretch | PetState::Groom | PetState::Eat => {
                self.tick += 1;
                if self.tick >= self.state_len {
                    self.enter_idle();
                }
            }
            PetState::Perch => {
                self.tick += 1;
                #[cfg(windows)]
                self.perch_follow();
                let done = self.perch.as_ref().map(|(_, _, t)| *t == 0).unwrap_or(true);
                if done {
                    // 从窗口上跳下来
                    self.perch = None;
                    self.state = PetState::Thrown;
                    self.tick = 0;
                    self.thrown_vel = (0.0, -0.1);
                    self.frame_at = Some(Instant::now() + Duration::from_millis(THROWN_FRAME_MS));
                    return;
                }
                window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
            }
            PetState::Climb => {
                self.tick += 1;
                if let Some(h) = self.hang_until {
                    // 在顶上挂一会儿
                    if Instant::now() >= h {
                        self.hang_until = None;
                        self.climb_vertical = 1;
                    }
                } else if self.climb_vertical < 0 {
                    self.pos.1 -= CLIMB_STEP;
                    if self.pos.1 <= self.mon.y + 2 {
                        self.pos.1 = self.mon.y + 2;
                        self.hang_until = Some(Instant::now() + Duration::from_millis(1200));
                    }
                } else {
                    self.pos.1 += CLIMB_STEP;
                    if self.pos.1 >= self.mon_bottom() {
                        self.pos.1 = self.mon_bottom();
                        // 落地后往屏幕里挪，结束爬墙
                        self.pos.0 = if self.climb_wall > 0 {
                            self.pos.0 - 12
                        } else {
                            self.pos.0 + 12
                        };
                        self.climb_wall = 0;
                        self.hang_until = None;
                        self.enter_idle();
                    }
                }
                window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
            }
            PetState::Thrown => {
                let dt = THROWN_FRAME_MS as f32;
                let (mut vx, mut vy) = self.thrown_vel;
                self.pos.0 += (vx * dt) as i32;
                self.pos.1 += (vy * dt) as i32;
                vy += GRAVITY * dt;
                let mut landed = false;
                if self.pos.0 < self.mon.x {
                    self.pos.0 = self.mon.x;
                    vx = -vx * 0.7;
                }
                if self.pos.0 > self.mon.x + self.mon.w - PET_SIZE {
                    self.pos.0 = self.mon.x + self.mon.w - PET_SIZE;
                    vx = -vx * 0.7;
                }
                if self.pos.1 < self.mon.y {
                    self.pos.1 = self.mon.y;
                    vy = 0.0;
                }
                let floor = self.mon_bottom();
                if self.pos.1 >= floor {
                    self.pos.1 = floor;
                    vy = -vy * 0.45;
                    vx *= 0.75;
                    if vy.abs() < 0.06 {
                        vy = 0.0;
                        landed = true;
                    }
                }
                self.thrown_vel = (vx, vy);
                window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
                if landed {
                    self.resolve_mon();
                    self.enter_idle();
                    self.reaction_end = Some(Instant::now() + Duration::from_millis(700));
                    self.state = PetState::Shocked;
                    self.tick = 0;
                    self.bubble_show("喵呜…晕了");
                }
            }
            PetState::Dragged | PetState::Patted | PetState::Shocked | PetState::Sleep => {
                self.tick += 1;
            }
        }
    }

    fn frame_duration(&self) -> Duration {
        match self.state {
            PetState::Walk | PetState::Climb => Duration::from_millis(WALK_FRAME_MS),
            PetState::Dragged => Duration::from_millis(DRAG_FRAME_MS),
            PetState::Thrown => Duration::from_millis(THROWN_FRAME_MS),
            PetState::Groom | PetState::Eat => Duration::from_millis(200),
            PetState::Stretch => Duration::from_millis(400),
            _ => Duration::from_millis(IDLE_FRAME_MS),
        }
    }

    fn resolve_mon(&mut self) {
        if let Some(m) = self.mons.iter().find(|m| m.contains_center(self.pos.0, self.pos.1, PET_SIZE)) {
            if m.x != self.mon.x || m.y != self.mon.y {
                self.mon = *m;
            }
        }
    }

    // ---------- 定时器 ----------

    fn next_wakeup(&self) -> Option<Instant> {
        let now = Instant::now();
        let press_deadline = self.press.map(|p| p.start + Duration::from_millis(DRAG_START_MS));
        [
            self.frame_at,
            self.bubble_until,
            self.reaction_end,
            self.whisper_at,
            self.typing_until,
            self.hang_until,
            self.drink_at,
            self.sit_at,
            press_deadline,
        ]
        .into_iter()
        .flatten()
        .filter(|t| *t > now)
        .min()
    }

    fn schedule(&mut self, el: &ActiveEventLoop) {
        match self.next_wakeup() {
            Some(t) => el.set_control_flow(ControlFlow::WaitUntil(t)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }

    // ---------- 气泡 ----------

    fn bubble_show(&mut self, text: &str) {
        if self.hidden {
            return; // 全屏遮挡期间绝不弹泡（游戏/视频零干扰）
        }
        let secs = (2 + text.chars().count() as u64 / 6).min(8);
        let above = self.pos.1 > self.mon.y + 90;
        if let Some(b) = &mut self.bubble {
            b.show(text, self.pos, self.mon, above);
            self.bubble_until = Some(Instant::now() + Duration::from_secs(secs.max(2)));
        }
    }

    /// 思考中动画泡：不设过期时间，回复到达时替换
    fn bubble_show_persistent(&mut self, text: &str) {
        if self.hidden {
            return;
        }
        let above = self.pos.1 > self.mon.y + 90;
        if let Some(b) = &mut self.bubble {
            b.show(text, self.pos, self.mon, above);
        }
        self.bubble_until = None;
    }

    fn bubble_hide(&mut self) {
        self.bubble_until = None;
        if let Some(b) = &mut self.bubble {
            b.hide();
        }
    }

    // ---------- 交互 ----------

    fn on_left_press(&mut self, window: &Window) {
        let now = Instant::now();
        if let Some((t, p)) = self.last_click {
            if now.duration_since(t) < Duration::from_millis(400)
                && (self.cursor.0 - p.0).abs() < 8.0
                && (self.cursor.1 - p.1).abs() < 8.0
            {
                self.last_click = None;
                self.press = None;
                if self.state == PetState::Sleep {
                    self.wake_with("喵呜…我醒啦");
                } else {
                    self.enter_sleep_by_bubble("Zzz…晚安");
                }
                window.request_redraw();
                return;
            }
        }
        self.last_click = Some((now, self.cursor));
        self.press_cursor = self.cursor;

        if self.state == PetState::Sleep {
            self.wake_with("喵呜…我醒啦");
            window.request_redraw();
            return;
        }
        self.press = Some(PressInfo { start: now, moved: false });
    }

    fn begin_drag(&mut self) {
        self.state = PetState::Dragged;
        self.tick = 0;
        self.drag_origin = self.pos;
        self.grab = (self.cursor.0 - self.pos.0 as f64, self.cursor.1 - self.pos.1 as f64);
        self.drag_track.clear();
        self.drag_track.push((Instant::now(), self.pos.0 as f64, self.pos.1 as f64));
        self.bubble_hide();
        self.frame_at = Some(Instant::now() + Duration::from_millis(DRAG_FRAME_MS));
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 从拖拽轨迹估算释放速度（像素/毫秒）
    fn drag_velocity(&self) -> (f32, f32) {
        let now = Instant::now();
        let recent: Vec<(Instant, f64, f64)> = self
            .drag_track
            .iter()
            .rev()
            .take_while(|(t, _, _)| now.duration_since(*t).as_millis() <= 120)
            .cloned()
            .collect();
        if recent.len() < 2 {
            return (0.0, 0.0);
        }
        let first = recent.first().unwrap();
        let last = recent.last().unwrap();
        let dt = last.0.duration_since(first.0).as_millis() as f32;
        if dt < 1.0 {
            return (0.0, 0.0);
        }
        (
            ((last.1 - first.1) / dt as f64) as f32,
            ((last.2 - first.2) / dt as f64) as f32,
        )
    }

    fn on_left_release(&mut self) {
        let press = self.press.take();
        if self.state == PetState::Dragged {
            let (vx, vy) = self.drag_velocity();
            let speed = (vx * vx + vy * vy).sqrt();
            if speed > FLING_SPEED_MIN {
                // 甩飞！
                self.state = PetState::Thrown;
                self.thrown_vel = (
                    vx.clamp(-1.3, 1.3),
                    vy.clamp(-1.3, 1.3) - 0.25, // 甩出时带点向上
                );
                self.frame_at = Some(Instant::now() + Duration::from_millis(THROWN_FRAME_MS));
                self.bubble_show("哇啊啊——！");
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
            let moved_px =
                (self.pos.0 - self.drag_origin.0).abs() + (self.pos.1 - self.drag_origin.1).abs();
            self.resolve_mon();
            self.enter_idle();
            self.frame_at = None;
            if moved_px > 24 {
                self.bubble_show("呼…安全着陆！");
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        if let Some(p) = press {
            if p.start.elapsed() < Duration::from_millis(CLICK_MAX_MS) && !p.moved {
                self.click_zone();
            }
        }
    }

    fn click_zone(&mut self) {
        let y = self.cursor.1 as i32;
        let (msg, dur) = if y < 20 {
            ("好舒服～再摸摸！", 1400u64)
        } else if y < 46 {
            ("呜哇！别戳肚子啦！", 1100)
        } else {
            ("喵嗷！踩到脚了！", 900)
        };
        self.state = if y < 20 { PetState::Patted } else { PetState::Shocked };
        self.tick = 0;
        self.reaction_end = Some(Instant::now() + Duration::from_millis(dur));
        self.frame_at = None;
        self.bubble_show(msg);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn feed(&mut self) {
        if self.state == PetState::Sleep {
            self.wake_with("闻到小鱼干味了！");
        }
        self.enter_pose(PetState::Eat, 10, Some("咔嚓咔嚓…小鱼干最棒了！"));
        let cfg = self.cfg.clone();
        tts::speak(&self.client, &cfg, self.settings.voice, "小鱼干最棒了喵！");
    }

    fn whisper(&mut self) {
        if self.settings.whisper_on
            && self.state == PetState::Idle
            && self.bubble_until.is_none()
            && !self.chat_pending
            && !self.hidden
        {
            let i = (self.rand() % WHISPERS.len() as u64) as usize;
            self.bubble_show(WHISPERS[i]);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        self.whisper_at = Some(Instant::now() + Duration::from_secs(self.rand_range(50, 140)));
    }

    // ---------- 绘制 ----------

    fn build_pose(&self) -> Pose {
        let typing = self.typing_until.map(|t| t > Instant::now()).unwrap_or(false)
            && (self.state == PetState::Idle || self.state == PetState::Walk);
        Pose {
            state: self.state,
            tick: self.tick,
            gaze: self.gaze(),
            expr: self.model.expression(),
            costume: self.model.costume(),
            aux: self.climb_wall,
            typing,
        }
    }

    fn draw_pet(&mut self, el: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else { return };
        self.advance(&window);
        let pose = self.build_pose();
        let sprite = self.model.render(&pose);
        let Some(surface) = &mut self.surface else { return };
        if let Ok(mut buf) = surface.buffer_mut() {
            for (dst, src) in buf.iter_mut().zip(sprite.iter()) {
                *dst = *src;
            }
            let _ = buf.present();
        }
        self.frame_at = Some(Instant::now() + self.frame_duration());
        self.schedule(el);
    }

    // ---------- 窗口 ----------

    /// 打开/重新贴位悬浮输入框
    fn ensure_input(&mut self, el: &ActiveEventLoop) {
        if let Some(i) = &mut self.input {
            i.open(self.pos, self.mon);
            return;
        }
        let ph = format!("和{}说点什么…", self.cfg.pet_name);
        if let Some(mut i) = InputBox::create(el, ph) {
            i.open(self.pos, self.mon);
            self.input = Some(i);
        }
    }

    /// 历史内存上限：50 条；3 条之前的图片数据直接剥离（base64 最大 4MB/张）
    fn trim_history(&mut self) {
        const MAX: usize = 50;
        const KEEP_IMAGES: usize = 3;
        if self.chat_history.len() > MAX {
            let drop = self.chat_history.len() - MAX;
            self.chat_history.drain(..drop);
        }
        let n = self.chat_history.len();
        for (i, m) in self.chat_history.iter_mut().enumerate() {
            if m.image.is_some() && i + KEEP_IMAGES < n {
                m.image = None;
            }
        }
    }

    /// 发送一条消息（文本或拖入的图片）：回复以气泡 + 语音呈现
    fn send_chat(&mut self, text: String, image: Option<String>) {
        self.chat_history
            .push(ChatMsg { role: Role::User, text, image: image.clone() });
        self.trim_history();
        let history = if self.cfg.ai_ready() {
            Some(build_history(&self.cfg, &self.chat_history))
        } else {
            None
        };
        if let Some(i) = &mut self.input {
            i.set_pending();
        }
        self.chat_pending = true;
        self.think_frame = 0;
        self.think_next = Some(Instant::now() + Duration::from_millis(350));
        match history {
            Some(h) => {
                // 思考中：头顶冒省略号跳动文字泡
                self.bubble_show_persistent("·");
                let cfg = self.cfg.clone();
                let model = if image.is_some() {
                    cfg.vision_model.clone().unwrap_or_else(|| cfg.model.clone())
                } else {
                    cfg.model.clone()
                };
                let client = self.client.clone();
                let proxy = self.proxy.clone();
                let voice = self.settings.voice;
                std::thread::spawn(move || {
                    let r = client.chat(&cfg, &model, &h);
                    if let (Ok(reply), true) = (&r, voice) {
                        // 语音在请求线程播，避免 ChatReply 里重复
                        tts::speak(&client, &cfg, true, reply);
                    }
                    let _ = proxy.send_event(PetEvent::ChatReply(r));
                });
            }
            None => {
                self.chat_pending = false;
                self.think_next = None;
                self.chat_history
                    .push(ChatMsg { role: Role::Pet, text: "还没接入 AI".into(), image: None });
                self.trim_history();
                self.bubble_show("喵呜～我还没接入 AI！在 deskpet.exe 旁边放一个 deskpet.toml（照抄 .example 填 api_key）就能聊天啦");
                if let Some(i) = &mut self.input {
                    i.clear_pending();
                }
            }
        }
    }

    fn ensure_todo(&mut self, el: &ActiveEventLoop) {
        if self.todo.is_some() {
            return;
        }
        if let Some(mut t) = TodoWin::create(el) {
            t.open(self.pos, self.mon);
            self.todo = Some(t);
        }
    }

    // ---------- 菜单 ----------

    /// 右键弹出自绘菜单（半透明圆角黑底白字）；再按一次右键 = 关闭
    #[cfg(windows)]
    fn open_pet_menu(&mut self, el: &ActiveEventLoop) {
        if self.menu.take().is_some() {
            return;
        }
        let pages = vec![
            (Page::Root, self.root_entries()),
            (Page::Fun, self.fun_entries()),
            (Page::Costume, self.costume_entries()),
            (Page::Expr, self.expr_entries()),
            (Page::Set, self.set_entries()),
        ];
        let at = cursor_pos().unwrap_or((self.pos.0 + 32, self.pos.1 + 32));
        if let Some(m) = MenuWin::create(el, pages, Page::Root) {
            self.menu = Some(m.open(at, self.mon));
        }
    }

    #[cfg(not(windows))]
    fn open_pet_menu(&mut self, _el: &ActiveEventLoop) {
        self.bubble_show("右键菜单只在 Windows 上有喵");
    }

    fn root_entries(&self) -> Vec<Entry> {
        let onoff = |on: bool| if on { "开" } else { "关" };
        vec![
            Entry::item("pet-chat", "对话"),
            Entry::item("pet-todo", "待办清单"),
            Entry::sep(),
            Entry::sub(Page::Fun, "▸ 互动"),
            Entry::sub(Page::Costume, "▸ 换装"),
            Entry::sub(Page::Expr, "▸ 表情"),
            Entry::sub(Page::Set, "▸ 设置"),
            Entry::sep(),
            Entry::stay("pet-remind", format!("提醒：{}", onoff(self.settings.drink_minutes > 0))),
            Entry::stay("set-voice", format!("语音播报：{}", onoff(self.settings.voice))),
            Entry::sep(),
            Entry::item(
                "pet-sleep",
                if self.state == PetState::Sleep { "叫醒" } else { "睡觉" },
            ),
            Entry::item("pet-about", "关于"),
            Entry::item("quit", "退出"),
        ]
    }

    fn fun_entries(&self) -> Vec<Entry> {
        vec![
            Entry::back(),
            Entry::stay("pet-feed", "喂小鱼干"),
            Entry::item("pet-perch", "趴到窗口上"),
            Entry::item("pet-throw", "扔个窗口"),
            Entry::stay("pet-say", "说句话"),
        ]
    }

    fn costume_entries(&self) -> Vec<Entry> {
        let cur = self.model.costume();
        let mut v: Vec<Entry> = self
            .model
            .info()
            .costumes
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Entry::stay(
                    &format!("costume-{i}"),
                    format!("{} {}", if i == cur { "●" } else { "○" }, name),
                )
            })
            .collect();
        v.push(Entry::back());
        v
    }

    fn expr_entries(&self) -> Vec<Entry> {
        let cur = self.model.expression();
        let mut v: Vec<Entry> = self
            .model
            .info()
            .expressions
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Entry::stay(
                    &format!("expr-{i}"),
                    format!("{} {}", if i == cur { "●" } else { "○" }, name),
                )
            })
            .collect();
        v.push(Entry::back());
        v
    }

    fn set_entries(&self) -> Vec<Entry> {
        let s = &self.settings;
        let onoff = |on: bool| if on { "开" } else { "关" };
        vec![
            Entry::back(),
            Entry::stay("set-keyboard", format!("键盘联动：{}", onoff(s.keyboard_link))),
            Entry::stay("set-gamepad", format!("手柄联动：{}", onoff(s.gamepad_link))),
            Entry::stay("set-gaze", format!("目光跟随：{}", onoff(s.gaze_follow))),
            Entry::stay("set-follow", format!("跟随鼠标：{}", onoff(s.follow_mouse))),
            Entry::stay("set-whisper", format!("随机碎碎念：{}", onoff(s.whisper_on))),
            Entry::stay("set-drink", format!("喝水提醒：{}", onoff(s.drink_minutes > 0))),
            Entry::stay("set-sit", format!("久坐提醒：{}", onoff(s.sit_minutes > 0))),
            Entry::sep(),
            Entry::item("pet-settings-help", "AI 参数请编辑 deskpet.toml"),
        ]
    }

    fn menu_action(&mut self, id: &str, el: &ActiveEventLoop) {
        match id {
            "quit" | "tray-quit" => el.exit(),
            "pet-chat" | "tray-chat" => self.ensure_input(el),
            "pet-todo" => self.ensure_todo(el),
            "pet-feed" => self.feed(),
            "pet-say" => {
                self.bubble_show("喵呜～喵呜喵呜！");
                let cfg = self.cfg.clone();
                tts::speak(&self.client, &cfg, self.settings.voice, "喵呜，喵呜喵呜！");
            }
            "pet-throw" => self.throw_a_window(),
            "pet-costume" => {
                let n = self.model.info().costumes.len();
                let next = (self.model.costume() + 1) % n;
                self.model.set_costume(next);
                let name = self.model.info().costumes[next].clone();
                self.bubble_show(&format!("变身为{name}！"));
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            "pet-remind" => {
                // 一键切换喝水+久坐提醒
                let enable = self.settings.drink_minutes == 0 || self.settings.sit_minutes == 0;
                self.settings.drink_minutes = if enable { 45 } else { 0 };
                self.settings.sit_minutes = if enable { 90 } else { 0 };
                self.arm_reminders();
                self.bubble_show(if enable { "提醒开好啦" } else { "提醒关掉了" });
                deskpet::config::persist_settings(&[
                    ("drink_minutes".into(), toml::Value::Integer(self.settings.drink_minutes as i64)),
                    ("sit_minutes".into(), toml::Value::Integer(self.settings.sit_minutes as i64)),
                ]);
            }
            "set-voice" => {
                self.settings.voice = !self.settings.voice;
                self.bubble_show(if self.settings.voice { "语音打开啦" } else { "语音关掉了" });
                self.persist_toggles();
            }
            "pet-sleep" => {
                if self.state == PetState::Sleep {
                    self.wake_with("喵呜…我醒啦");
                } else {
                    self.enter_sleep_by_bubble("Zzz…晚安");
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            "set-keyboard" => {
                self.settings.keyboard_link = !self.settings.keyboard_link;
                self.bubble_show(if self.settings.keyboard_link { "键盘联动开" } else { "键盘联动关" });
                self.persist_toggles();
            }
            "set-gamepad" => {
                self.settings.gamepad_link = !self.settings.gamepad_link;
                self.bubble_show(if self.settings.gamepad_link { "手柄联动开" } else { "手柄联动关" });
                self.persist_toggles();
            }
            "set-gaze" => {
                self.settings.gaze_follow = !self.settings.gaze_follow;
                self.bubble_show(if self.settings.gaze_follow { "目光跟随开" } else { "目光跟随关" });
                self.persist_toggles();
            }
            "set-follow" => {
                self.settings.follow_mouse = !self.settings.follow_mouse;
                self.bubble_show(if self.settings.follow_mouse { "跟着你走啦！" } else { "不去追鼠标了" });
                self.persist_toggles();
            }
            "set-whisper" => {
                self.settings.whisper_on = !self.settings.whisper_on;
                self.bubble_show(if self.settings.whisper_on { "碎碎念开" } else { "碎碎念关" });
                self.persist_toggles();
            }
            "set-drink" => {
                self.settings.drink_minutes = if self.settings.drink_minutes == 0 { 45 } else { 0 };
                self.arm_reminders();
                self.bubble_show(if self.settings.drink_minutes > 0 { "喝水提醒开" } else { "喝水提醒关" });
                self.persist_toggles();
            }
            "set-sit" => {
                self.settings.sit_minutes = if self.settings.sit_minutes == 0 { 90 } else { 0 };
                self.arm_reminders();
                self.bubble_show(if self.settings.sit_minutes > 0 { "久坐提醒开" } else { "久坐提醒关" });
                self.persist_toggles();
            }
            "pet-settings-help" => {
                self.bubble_show("AI 参数在 deskpet.toml 里改喵");
            }
            "pet-about" => {
                self.bubble_show(&format!(
                    "{} v0.3.2 · 纯Rust桌宠\n右键→对话可接AI聊天\n设置里可开关功能",
                    self.model.info().name
                ));
            }
            "pet-perch" => self.perch_on_window(),
            other => {
                if let Some(n) = other.strip_prefix("costume-") {
                    if let Ok(i) = n.parse::<usize>() {
                        if i < self.model.info().costumes.len() {
                            self.model.set_costume(i);
                            let name = self.model.info().costumes[i].clone();
                            self.bubble_show(&format!("变身为{name}！"));
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                } else if let Some(n) = other.strip_prefix("expr-") {
                    if let Ok(i) = n.parse::<usize>() {
                        self.model.set_expression(i);
                    }
                }
            }
        }
    }

    fn persist_toggles(&self) {
        let st = &self.settings;
        deskpet::config::persist_settings(&[
            ("voice".into(), toml::Value::Boolean(st.voice)),
            ("drink_minutes".into(), toml::Value::Integer(st.drink_minutes as i64)),
            ("sit_minutes".into(), toml::Value::Integer(st.sit_minutes as i64)),
            ("keyboard_link".into(), toml::Value::Boolean(st.keyboard_link)),
            ("gamepad_link".into(), toml::Value::Boolean(st.gamepad_link)),
            ("gaze_follow".into(), toml::Value::Boolean(st.gaze_follow)),
            ("follow_mouse".into(), toml::Value::Boolean(st.follow_mouse)),
            ("whisper_on".into(), toml::Value::Boolean(st.whisper_on)),
        ]);
    }

    // ---------- 提醒 ----------

    fn arm_reminders(&mut self) {
        let now = Instant::now();
        self.drink_at = if self.settings.drink_minutes > 0 {
            Some(now + Duration::from_secs(self.settings.drink_minutes * 60))
        } else {
            None
        };
        self.sit_at = if self.settings.sit_minutes > 0 {
            Some(now + Duration::from_secs(self.settings.sit_minutes * 60))
        } else {
            None
        };
    }

    fn fire_reminder(&mut self, drink: bool) {
        let (msg, tts_text) = if drink {
            ("叮咚——该喝水啦！", "主人，该喝水啦")
        } else {
            ("坐好久啦，起来伸个懒腰喵！", "主人，坐久了，起来活动一下吧")
        };
        if self.hidden {
            // 全屏遮挡：静默顺延，不弹泡不出声
            self.arm_reminders();
            return;
        }
        if self.state != PetState::Sleep {
            self.state = PetState::Patted;
            self.tick = 0;
            self.reaction_end = Some(Instant::now() + Duration::from_millis(1500));
        }
        self.bubble_show(msg);
        let cfg = self.cfg.clone();
        tts::speak(&self.client, &cfg, self.settings.voice, tts_text);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.arm_reminders();
    }
}


impl ApplicationHandler<PetEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes()
            .with_inner_size(PhysicalSize::new(PET_SIZE as u32, PET_SIZE as u32))
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_active(false)
            .with_title("deskpet");
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                dlog(&format!("失败：创建窗口 {e}"));
                el.exit();
                return;
            }
        };
        dlog("窗口创建成功");

        // 多屏：收集所有显示器，初始用窗口所在的那块
        self.mons = el
            .available_monitors()
            .into_iter()
            .map(|m| MonRect {
                x: m.position().x,
                y: m.position().y,
                w: m.size().width as i32,
                h: m.size().height as i32,
            })
            .filter(|m| m.w > 0 && m.h > 0)
            .collect();
        if self.mons.is_empty() {
            self.mons = vec![MonRect { x: 0, y: 0, w: 1280, h: 720 }];
        }
        if let Some(cm) = window.current_monitor() {
            let (cx, cy) = (cm.position().x, cm.position().y);
            if let Some(m) = self.mons.iter().find(|m| m.x == cx && m.y == cy) {
                self.mon = *m;
            }
        }

        self.pos = (
            (self.mon.x + self.mon.w - PET_SIZE - 48).max(self.mon.x),
            (self.mon.y + self.mon.h - PET_SIZE - 96).max(self.mon.y),
        );
        window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));

        let ctx = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                dlog(&format!("失败：softbuffer 上下文 {e}"));
                el.exit();
                return;
            }
        };
        let mut surface = match softbuffer::Surface::new(&ctx, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                dlog(&format!("失败：softbuffer surface {e}"));
                el.exit();
                return;
            }
        };
        dlog("渲染就绪（softbuffer）");
        if surface
            .resize(
                NonZeroU32::new(PET_SIZE as u32).unwrap(),
                NonZeroU32::new(PET_SIZE as u32).unwrap(),
            )
            .is_err()
        {
            el.exit();
            return;
        }
        self.surface = Some(surface);
        self.window = Some(window.clone());
        self.bubble = BubbleWin::create(el);
        dlog(&format!(
            "显示器 {} 块，当前 {}x{}+{}+{}；气泡窗 {}",
            self.mons.len(),
            self.mon.w, self.mon.h, self.mon.x, self.mon.y,
            if self.bubble.is_some() { "OK" } else { "创建失败" }
        ));

        self.whisper_at = Some(Instant::now() + Duration::from_secs(25));
        self.arm_reminders();

        #[cfg(windows)]
        {
            if let Ok(t) = create_tray() {
                self.tray = Some(t);
                dlog("托盘创建成功");
            } else {
                dlog("托盘创建失败（不影响其他功能）");
            }
            if self.settings.keyboard_link {
                input::win::spawn_keyboard_hook(self.proxy.clone());
                dlog("键盘钩子已请求安装");
            }
            if self.settings.gamepad_link {
                input::win::spawn_gamepad_poll(self.proxy.clone());
                dlog("手柄轮询已启动");
            }
        }

        window.request_redraw();
        dlog("启动完成");
    }

    fn window_event(&mut self, el: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        // 右键菜单窗口事件路由
        if let Some(menu) = &mut self.menu {
            if menu.window.id() == window_id {
                let outcome = match &event {
                    WindowEvent::RedrawRequested => {
                        menu.draw();
                        MenuOutcome::None
                    }
                    e => menu.handle(e),
                };
                match outcome {
                    MenuOutcome::None => {}
                    MenuOutcome::Close => self.menu = None,
                    MenuOutcome::Action { id, stay } => {
                        self.menu_action(&id, el);
                        if !stay {
                            self.menu = None;
                        } else {
                            // stay 类动作（换装/表情/开关）处理后刷新标签，菜单保持打开
                            let mut menu = self.menu.take().unwrap();
                            let entries = match menu.page() {
                                Page::Root => self.root_entries(),
                                Page::Fun => self.fun_entries(),
                                Page::Costume => self.costume_entries(),
                                Page::Expr => self.expr_entries(),
                                Page::Set => self.set_entries(),
                            };
                            menu.set_entries(entries);
                            self.menu = Some(menu);
                        }
                    }
                }
                return;
            }
        }

        // 悬浮输入框事件路由
        if let Some(input) = &mut self.input {
            if input.window.id() == window_id {
                let action = match &event {
                    WindowEvent::RedrawRequested => {
                        input.draw();
                        InputAction::None
                    }
                    e => input.handle(e),
                };
                match action {
                    InputAction::None => {}
                    InputAction::Close => self.input = None,
                    InputAction::Send { text, image } => self.send_chat(text, image),
                }
                return;
            }
        }
        // 待办窗事件路由
        if let Some(todo) = &mut self.todo {
            if todo.window.id() == window_id {
                let action = match &event {
                    WindowEvent::RedrawRequested => {
                        todo.draw();
                        TodoAction::None
                    }
                    e => todo.handle(e),
                };
                if matches!(action, TodoAction::Close) {
                    self.todo = None;
                }
                return;
            }
        }

        let Some(window) = self.window.clone() else { return };
        if window.id() != window_id {
            return;
        }
        match event {
            WindowEvent::RedrawRequested => self.draw_pet(el),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.on_left_press(&window);
                self.schedule(el);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                self.open_pet_menu(el);
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.on_left_release();
                self.schedule(el);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if self.state == PetState::Dragged {
                    if let Some(p) = &self.press {
                        if p.moved || p.start.elapsed() >= Duration::from_millis(DRAG_START_MS) {
                            let nx = (self.cursor.0 - self.grab.0) as i32;
                            let ny = (self.cursor.1 - self.grab.1) as i32;
                            // 允许跨屏拖动：夹到所有显示器的联合包围盒
                            let (min_x, min_y) = self
                                .mons
                                .iter()
                                .fold((i32::MAX, i32::MAX), |a, m| (a.0.min(m.x), a.1.min(m.y)));
                            let (max_x, max_y) = self.mons.iter().fold(
                                (i32::MIN, i32::MIN),
                                |a, m| (a.0.max(m.x + m.w - PET_SIZE), a.1.max(m.y + m.h - PET_SIZE)),
                            );
                            self.pos = (nx.clamp(min_x, max_x), ny.clamp(min_y, max_y));
                            window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
                            self.drag_track
                                .push((Instant::now(), self.pos.0 as f64, self.pos.1 as f64));
                            if self.drag_track.len() > 16 {
                                self.drag_track.remove(0);
                            }
                        }
                    }
                } else if let Some(p) = &mut self.press {
                    let (lx, ly) = (self.cursor.0, self.cursor.1);
                    if (lx - self.press_cursor.0).abs() > 6.0 || (ly - self.press_cursor.1).abs() > 6.0 {
                        p.moved = true;
                        // 移动超过阈值直接进入拖拽（不必等长按）
                        if p.start.elapsed() >= Duration::from_millis(120) {
                            self.begin_drag();
                        }
                    }
                }
            }
            WindowEvent::Ime(Ime::Enabled) | WindowEvent::Ime(Ime::Disabled) => {}
            WindowEvent::CloseRequested => el.exit(),
            _ => {}
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: PetEvent) {
        match event {
            PetEvent::FullscreenChanged(fs) => {
                self.hidden = fs;
                if let Some(w) = &self.window {
                    w.set_visible(!fs);
                    if fs {
                        self.menu = None;
                        // 瞬态状态立即落地，避免隐藏期间动画停摆、恢复后冻结
                        self.press = None;
                        if matches!(
                            self.state,
                            PetState::Thrown
                                | PetState::Climb
                                | PetState::Dragged
                                | PetState::Perch
                        ) {
                            self.perch = None;
                            self.pos.1 = self.mon_bottom();
                            w.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
                            self.hang_until = None;
                            self.climb_wall = 0;
                            self.enter_idle();
                        }
                        self.bubble_hide();
                        if let Some(i) = &self.input {
                            i.window.set_visible(false);
                        }
                        if let Some(t) = &self.todo {
                            t.window.set_visible(false);
                        }
                        self.frame_at = None;
                        self.typing_until = None;
                        el.set_control_flow(ControlFlow::Wait);
                        return;
                    }
                    if let Some(i) = &self.input {
                        i.window.set_visible(true);
                    }
                    if let Some(t) = &self.todo {
                        t.window.set_visible(true);
                    }
                    w.request_redraw();
                }
            }
            PetEvent::ChatReply(r) => {
                self.chat_pending = false;
                self.think_next = None;
                let reply = match r {
                    Ok(t) => t,
                    Err(e) => format!("出错了：{e}"),
                };
                self.chat_history
                    .push(ChatMsg { role: Role::Pet, text: reply.clone(), image: None });
                self.trim_history();
                let shown: String = reply.chars().take(120).collect();
                self.bubble_show(&shown);
                if let Some(i) = &mut self.input {
                    i.clear_pending();
                }
            }
            PetEvent::Typing => {
                if self.settings.keyboard_link
                    && !self.hidden
                    && (self.state == PetState::Idle || self.state == PetState::Walk)
                {
                    self.typing_until = Some(Instant::now() + Duration::from_millis(150));
                    let early = Instant::now() + Duration::from_millis(100);
                    if self.frame_at.map(|t| t > early).unwrap_or(true) {
                        self.frame_at = Some(early);
                    }
                }
            }
            PetEvent::Gamepad => {
                if self.settings.gamepad_link
                    && !self.hidden
                    && self.state != PetState::Sleep
                    && self.state != PetState::Dragged
                    && self.state != PetState::Thrown
                {
                    self.state = PetState::Patted;
                    self.tick = 0;
                    self.reaction_end = Some(Instant::now() + Duration::from_millis(800));
                    self.frame_at = None;
                    let i = (self.rand() % PAD_REACTIONS.len() as u64) as usize;
                    self.bubble_show(PAD_REACTIONS[i]);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
        }
        self.schedule(el);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // 托盘 / 右键菜单事件（muda 全局通道）
        #[cfg(windows)]
        while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            self.menu_action(&ev.id().0, el);
        }

        let now = Instant::now();
        // 长按 → 开始拖拽
        if let Some(p) = self.press {
            if self.state != PetState::Dragged
                && self.state != PetState::Sleep
                && now.duration_since(p.start) >= Duration::from_millis(DRAG_START_MS)
            {
                self.begin_drag();
            }
        }
        // 气泡到期
        if let Some(t) = self.bubble_until {
            if now >= t {
                self.bubble_hide();
            }
        }
        // 反应表情结束
        if let Some(t) = self.reaction_end {
            if now >= t
                && (self.state == PetState::Patted
                    || self.state == PetState::Shocked)
            {
                self.enter_idle();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
        // 爬墙顶上挂完
        if let Some(t) = self.hang_until {
            if now >= t && self.state == PetState::Climb {
                self.hang_until = None;
                self.climb_vertical = 1;
                self.frame_at = Some(now + Duration::from_millis(WALK_FRAME_MS));
            }
        }
        // 动画帧到期
        if let Some(t) = self.frame_at {
            if now >= t {
                self.frame_at = None;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
        // 思考中省略号动画
        if self.chat_pending {
            if let Some(t) = self.think_next {
                if now >= t {
                    self.think_frame = (self.think_frame + 1) % 3;
                    self.think_next = Some(now + Duration::from_millis(350));
                    let dots = format!("{}{}", "·".repeat(self.think_frame as usize + 1), " ".repeat(2 - self.think_frame as usize));
                    if let Some(b) = &mut self.bubble {
                        let above = self.pos.1 > self.mon.y + 90;
                        b.show(&dots, self.pos, self.mon, above);
                    }
                }
            }
        }
        // 随机碎碎念
        if let Some(t) = self.whisper_at {
            if now >= t {
                self.whisper_at = None;
                self.whisper();
            }
        }
        // 喝水/久坐提醒
        if let Some(t) = self.drink_at {
            if now >= t {
                self.drink_at = None;
                self.fire_reminder(true);
            }
        }
        if let Some(t) = self.sit_at {
            if now >= t {
                self.sit_at = None;
                self.fire_reminder(false);
            }
        }
        self.schedule(el);
    }
}


#[cfg(not(windows))]
impl App {
    fn throw_a_window(&mut self) {
        self.bubble_show("扔窗口只在 Windows 上有喵");
    }
    fn perch_on_window(&mut self) {
        self.bubble_show("趴窗口只在 Windows 上有喵");
    }
}

#[cfg(windows)]
impl App {
    /// 趴到别的窗口顶边上：随机挑一个候选窗口，跟随其位置
    fn perch_on_window(&mut self) {
        let cands = self.candidate_windows();
        if cands.is_empty() {
            self.bubble_show("没有可以趴的窗口喵");
            return;
        }
        let i = (self.rand() % cands.len() as u64) as usize;
        let (hwnd, rect) = cands[i];
        let addr = hwnd.0 as isize;
        let w = (rect.right - rect.left).max(PET_SIZE + 8);
        let off = 4 + (self.rand() % ((w - PET_SIZE - 8).max(1) as u64)) as i32;
        self.perch = Some((addr, off, 40)); // ~7 秒
        self.enter_pose(PetState::Perch, u32::MAX, Some("爬上来啦～"));
        self.perch_follow();
    }

    /// 每帧跟随目标窗口的位置；窗口没了就跳下去
    fn perch_follow(&mut self) {
        use windows::Win32::Foundation::{HWND, RECT};
        use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsWindow};
        let Some((addr, off, ticks)) = &mut self.perch else { return };
        let hwnd = HWND(*addr as *mut core::ffi::c_void);
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() {
                *ticks = 0;
                return;
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                *ticks = 0;
                return;
            }
            self.pos.0 = (rect.left + *off).max(self.mon.x);
            self.pos.1 = (rect.top - PET_SIZE + 3).max(self.mon.y);
        }
    }
}

#[cfg(windows)]
fn cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt).ok().map(|_| (pt.x, pt.y)) }
}

/// 全屏检测：前台窗口是否铺满所在显示器（带 4px 容差）。
#[cfg(windows)]
unsafe fn foreground_is_fullscreen() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowRect};

    let hwnd = GetForegroundWindow();
    if hwnd.is_invalid() {
        return false;
    }
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect).is_err() {
        return false;
    }
    let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if hmon.is_invalid() {
        return false;
    }
    let mut info = MONITORINFO::default();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if !GetMonitorInfoW(hmon, &mut info).as_bool() {
        return false;
    }
    let m = info.rcMonitor;
    let w = (rect.right - rect.left) as i64;
    let h = (rect.bottom - rect.top) as i64;
    let mw = (m.right - m.left) as i64;
    let mh = (m.bottom - m.top) as i64;
    w + 4 >= mw && h + 4 >= mh
}

#[cfg(windows)]
fn spawn_fullscreen_watcher(proxy: PetEventProxy) {
    std::thread::spawn(move || {
        let mut was = false;
        loop {
            let fs = unsafe { foreground_is_fullscreen() };
            if fs != was {
                was = fs;
                let _ = proxy.send_event(PetEvent::FullscreenChanged(fs));
            }
            std::thread::sleep(Duration::from_millis(2000));
        }
    });
}

#[cfg(not(windows))]
fn spawn_fullscreen_watcher(_proxy: PetEventProxy) {}

impl App {
    /// 把别人的窗口"扔"出去：枚举候选 → 抛物线动画（独立线程，MoveWindow 每 16ms 一步）
        /// 枚举可交互的候选窗口（可见、有标题、非自身、非最大化、非 cloaked）
    #[cfg(windows)]
    fn candidate_windows(&self) -> Vec<(windows::Win32::Foundation::HWND, windows::Win32::Foundation::RECT)> {
        use windows::core::BOOL;
        use windows::Win32::Foundation::{HWND, LPARAM, RECT};
        use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
        use windows::Win32::System::Threading::GetCurrentProcessId;
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowPlacement, GetWindowRect, GetWindowTextLengthW,
            GetWindowThreadProcessId, IsWindowVisible, SW_SHOWMAXIMIZED, WINDOWPLACEMENT,
        };

        unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let list = &mut *(lparam.0 as *mut Vec<HWND>);
            list.push(hwnd);
            true.into()
        }

        let mut all: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(enum_cb), LPARAM(&mut all as *mut _ as isize));
        }
        let mypid = unsafe { GetCurrentProcessId() };
        let mon = self.mon;
        let mut out = Vec::new();
        for hwnd in all {
            unsafe {
                if !IsWindowVisible(hwnd).as_bool() {
                    continue;
                }
                if GetWindowTextLengthW(hwnd) == 0 {
                    continue;
                }
                if GetWindowThreadProcessId(hwnd, None) == mypid {
                    continue;
                }
                let mut place = WINDOWPLACEMENT::default();
                place.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
                if GetWindowPlacement(hwnd, &mut place).is_ok()
                    && place.showCmd == SW_SHOWMAXIMIZED.0 as u32
                {
                    continue;
                }
                // 跳过被 DWM 遮蔽的不可见 UWP 窗口（Alt+Tab 里看不到的那种）
                let mut cloaked = 0u32;
                if DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED,
                    &mut cloaked as *mut u32 as *mut _,
                    std::mem::size_of::<u32>() as u32,
                )
                .is_ok()
                    && cloaked != 0
                {
                    continue;
                }
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_err() {
                    continue;
                }
                let mut cls = [0u16; 32];
                let n = GetClassNameW(hwnd, &mut cls);
                let cls = String::from_utf16_lossy(&cls[..n as usize]);
                if matches!(
                    cls.as_str(),
                    "Shell_TrayWnd" | "Progman" | "WorkerW" | "Windows.UI.Core.CoreWindow"
                ) {
                    continue;
                }
                let w = (rect.right - rect.left) as i64;
                let h = (rect.bottom - rect.top) as i64;
                if w <= 0 || h <= 0 || (w >= mon.w as i64 && h >= mon.h as i64) {
                    continue;
                }
                out.push((hwnd, rect));
            }
        }
        out
    }

#[cfg(windows)]
    fn throw_a_window(&mut self) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        };

        let cands = self.candidate_windows();
        let best = cands
            .iter()
            .max_by_key(|(_, r)| (r.right - r.left) as i64 * (r.bottom - r.top) as i64)
            .cloned();
        let Some((hwnd, rect)) = best else {
            self.bubble_show("没有可以扔的窗口喵");
            return;
        };
        let hwnd_addr = hwnd.0 as isize;
        let mon = self.mon;
        self.bubble_show("接住——！");
        let dir = if self.rand() % 2 == 0 { 1.0f32 } else { -1.0f32 };
        let vx0 = dir * (0.5 + (self.rand() % 40) as f32 / 100.0);
        let vy0 = -(0.8 + (self.rand() % 40) as f32 / 100.0);
        std::thread::spawn(move || unsafe {
            let hwnd = HWND(hwnd_addr as *mut core::ffi::c_void);
            let mut x = rect.left as f32;
            let mut y = rect.top as f32;
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;
            let (mut vx, mut vy) = (vx0, vy0);
            let floor = (mon.y + mon.h - h) as f32;
            for _ in 0..150 {
                x += vx * 16.0;
                y += vy * 16.0;
                vy += GRAVITY * 16.0;
                if y >= floor {
                    y = floor;
                    vy = -vy * 0.4;
                    vx *= 0.8;
                    if vy.abs() < 0.08 {
                        break;
                    }
                }
                if x < mon.x as f32 {
                    x = mon.x as f32;
                    vx = -vx * 0.8;
                }
                if x > (mon.x + mon.w - w) as f32 {
                    x = (mon.x + mon.w - w) as f32;
                    vx = -vx * 0.8;
                }
                // SWP_NOACTIVATE|SWP_NOZORDER：动画期间不抢焦点、不变动 z 序
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    x as i32,
                    y as i32,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
                );
                std::thread::sleep(Duration::from_millis(16));
            }
            let _ = SetWindowPos(
                hwnd,
                None,
                x as i32,
                (y as i32).clamp(mon.y, mon.y + mon.h - h),
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
            );
        });
    }
}

#[cfg(windows)]
fn create_tray() -> Result<tray_icon::TrayIcon, Box<dyn std::error::Error>> {
    use tray_icon::menu::{Menu, MenuItem};
    let rgba = sprites::tray_icon_rgba();
    let icon = tray_icon::Icon::from_rgba(rgba, 32, 32)?;
    let menu = Menu::new();
    menu.append(&MenuItem::with_id("tray-chat", "对话", true, None))?;
    menu.append(&MenuItem::with_id("tray-quit", "退出", true, None))?;
    let tray = tray_icon::TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("deskpet（拖动 / 双击睡觉 / 右键菜单）")
        .with_icon(icon)
        .build()?;
    Ok(tray)
}

/// 启动诊断日志：写到 exe 旁 deskpet.log（不可写则退 %TEMP%）
fn dlog(msg: &str) {
    use std::io::Write;
    let path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("deskpet.log")))
        .unwrap_or_else(|| std::env::temp_dir().join("deskpet.log"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        dlog(&format!("PANIC: {info}"));
    }));
    dlog("启动：v0.3.2");
    let text = deskpet::config::load_text();
    let cfg = text.as_deref().map(deskpet::config::parse_config).unwrap_or_default();
    let settings = deskpet::config::parse_settings(text.as_deref().unwrap_or(""));
    dlog(if cfg.ai_ready() {
        "配置已加载：AI 已配置"
    } else {
        "配置未找到或未配密钥：离线模式"
    });
    let event_loop = EventLoop::<PetEvent>::with_user_event()
        .build()
        .expect("创建事件循环失败");
    let proxy = event_loop.create_proxy();
    spawn_fullscreen_watcher(proxy.clone());
    let mut app = App::new(cfg, settings, proxy);
    event_loop.run_app(&mut app).expect("事件循环异常退出");
}
