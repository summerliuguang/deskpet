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

use deskpet::for_each_setting;
use deskpet::{
    ai::{build_history, ChatErr, ChatMsg, Client, Role},
    bubble::BubbleWin,
    config::{Config, Settings},
    dwarn,
    inputbox::{InputAction, InputBox},
    sprite_model::SpriteModel,
    menu::{Entry, MenuOutcome, MenuWin, Page},
    model::{make_model, model_names, PetModel, PetState, Pose},
    todo::{TodoAction, TodoWin},
    tts, dlog, MonRect, PetEvent, PetEventProxy, SbSurface,
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

/// 定时任务种类（每种一个槽位）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Deadline {
    /// 气泡到期隐藏
    BubbleHide,
    /// 反应表情（摸头/惊吓/手柄）结束
    ReactionEnd,
    /// 随机碎碎念
    Whisper,
    /// 爬墙顶上挂一会儿
    Hang,
    /// 喝水提醒
    Drink,
    /// 久坐提醒
    Sit,
    /// 周期保存会话（recur 300s）
    Autosave,
    /// 待办截止轮询（recur 20s）
    DueCheck,
    /// 启动消息（配置诊断/首次引导）
    Onboard,
    /// 思考中省略号动画（recur 350ms）
    Think,
}

/// 统一定时器：此前 10 个 Option<Instant> 各要手改 4 处
/// （字段/构造/next_wakeup/about_to_wait），漏掉 next_wakeup 就是
/// 事件循环睡死的静默 bug——现在只加枚举项 + fire 分支 + set 调用。
struct Timers {
    /// 每类一个槽：时刻 + 周期（Some = 触发后自动顺延，防积压从 now 起算）
    slots: [Option<(Instant, Option<Duration>)>; 10],
}

impl Timers {
    fn new() -> Self {
        Self { slots: [None; 10] }
    }

    fn set(&mut self, d: Deadline, at: Instant) {
        self.slots[d as usize] = Some((at, None));
    }

    fn set_recur(&mut self, d: Deadline, first: Instant, period: Duration) {
        self.slots[d as usize] = Some((first, Some(period)));
    }

    fn cancel(&mut self, d: Deadline) {
        self.slots[d as usize] = None;
    }

    fn is_set(&self, d: Deadline) -> bool {
        self.slots[d as usize].is_some()
    }

    /// 取出最早到期任务；recur 自动顺延到未来（单次自动清槽）
    fn pop_due(&mut self, now: Instant) -> Option<Deadline> {
        let mut best: Option<(Instant, usize)> = None;
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some((at, _)) = slot {
                if *at <= now && best.map(|(bt, _)| *at < bt).unwrap_or(true) {
                    best = Some((*at, i));
                }
            }
        }
        let (_, i) = best?;
        let (at, recur) = self.slots[i].unwrap();
        match recur {
            Some(period) => {
                let mut next = at + period;
                while next <= now {
                    next += period;
                }
                self.slots[i] = Some((next, Some(period)));
            }
            None => self.slots[i] = None,
        }
        Some(match i {
            0 => Deadline::BubbleHide,
            1 => Deadline::ReactionEnd,
            2 => Deadline::Whisper,
            3 => Deadline::Hang,
            4 => Deadline::Drink,
            5 => Deadline::Sit,
            6 => Deadline::Autosave,
            7 => Deadline::DueCheck,
            8 => Deadline::Onboard,
            _ => Deadline::Think,
        })
    }

    /// 全部未来时刻（供 next_wakeup 汇总）
    fn iter_times(&self) -> impl Iterator<Item = Instant> + '_ {
        let now = Instant::now();
        self.slots
            .iter()
            .filter_map(|s| s.map(|(t, _)| t))
            .filter(move |t| *t > now)
    }
}

/// 模型来源
#[derive(Clone)]
pub enum ModelSource {
    Builtin(usize),
    Sprite(std::path::PathBuf),
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
    model_kind: usize,
    /// 宠物窗口边长（物理像素，来自 model.size()）
    pet_size: i32,
    /// 模型注册表：(显示名, 来源)
    models: Vec<(String, ModelSource)>,

    state: PetState,
    tick: u32,
    /// 当前状态的结束时刻（None = 不自动结束，如睡/拖/飞/趴）；
    /// tick 只作精灵动画相位，行为节奏由真实时间驱动（掉帧不再变慢）
    state_until: Option<Instant>,
    idle_cycles: u32,
    dir: i32,
    pos: (i32, i32),
    /// 当前所在显示器（Wander/爬墙/弹跳边界）
    mon: MonRect,
    mons: Vec<MonRect>,
    hidden: bool,

    frame_at: Option<Instant>,
    /// 统一定时器（气泡/反应/碎碎念/挂墙/提醒/周期任务）
    timers: Timers,
    typing_until: Option<Instant>,
    press: Option<PressInfo>,
    press_cursor: (f64, f64),
    drag_origin: (i32, i32),
    /// 按下时的鼠标本地坐标 == 全局抓取偏移（按下时窗口未动，
    /// local = global - win_pos 退化为 local = 抓取偏移），拖动全程锚定它
    grab: (f64, f64),
    drag_track: Vec<(Instant, f64, f64)>,
    thrown_vel: (f32, f32),
    climb_wall: i32,
    climb_vertical: i32,
    /// 上一次"短点击"松开的时刻与位置（双击判定锚点：
    /// 锚在松开而不是按下，单击后立刻按住拖动不会误判成双击）
    last_release: Option<(Instant, (f64, f64))>,
    cursor: (f64, f64),
    rng: u64,

    /// AI 对话历史（气泡模式无历史 UI，但上下文保留在内存）
    chat_history: Vec<ChatMsg>,
    chat_pending: bool,
    think_frame: u32,
    /// 趴窗状态：(窗口句柄地址, 相对窗口左缘的偏移, 剩余 tick)
    perch: Option<(isize, i32, u32)>,
    /// 启动诊断提示（配置文件问题），resumed 后弹一次
    startup_diag: Option<String>,
    /// --selftest 冒烟模式：创建全部窗口渲染一帧后自动退出
    selftest: bool,
    /// 冒烟模式中已完成渲染探活的窗口
    selftest_seen: Vec<&'static str>,
    selftest_deadline: Option<Instant>,
}

impl App {
    fn new(cfg: Config, settings: Settings, proxy: PetEventProxy, startup_diag: Option<String>) -> Self {
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
            model: make_model(0),
            model_kind: 0,
            pet_size: 64,
            models: Vec::new(),
            state: PetState::Idle,
            tick: 0,
            state_until: None,
            idle_cycles: 0,
            dir: 1,
            pos: (0, 0),
            mon: MonRect { x: 0, y: 0, w: 1280, h: 720 },
            mons: vec![MonRect { x: 0, y: 0, w: 1280, h: 720 }],
            hidden: false,
            frame_at: None,
            timers: Timers::new(),
            typing_until: None,
            press: None,
            press_cursor: (0.0, 0.0),
            drag_origin: (0, 0),
            grab: (0.0, 0.0),
            drag_track: Vec::new(),
            thrown_vel: (0.0, 0.0),
            climb_wall: 0,
            climb_vertical: 0,
            last_release: None,
            cursor: (0.0, 0.0),
            rng: 0x9E3779B97F4A7C15,
            chat_history: Vec::new(),
            chat_pending: false,
            think_frame: 0,
            perch: None,
            startup_diag,
            selftest: false,
            selftest_seen: Vec::new(),
            selftest_deadline: None,
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
        self.mon.y + self.mon.h - self.pet_size
    }

    /// 按体型比例的移动步长（64px 猫 = 4px/帧，512px 猫 = 32px/帧）
    fn walk_step(&self) -> i32 {
        (self.pet_size / 16).max(2)
    }

    fn climb_step(&self) -> i32 {
        (self.pet_size / 32).max(1)
    }

    // ---------- 状态机 ----------

    /// 状态切换唯一入口：统一重置 tick 与状态时长；Idle 睡意计数按原语义
    /// 维护（重进 Idle 清零）。瞬态计时（frame_at/reaction_end/hang_until）
    /// 由调用方按需清理——这里刻意不碰。
    fn transition(&mut self, next: PetState, dur: Option<Duration>) {
        if next == PetState::Idle && self.state != PetState::Idle {
            self.idle_cycles = 0;
        }
        self.state = next;
        self.tick = 0;
        self.state_until = dur.map(|d| Instant::now() + d);
    }

    fn enter_idle(&mut self) {
        // 时长与旧"帧数×帧间隔"等价：30~90 tick × 600ms
        let dur = Duration::from_millis(self.rand_range(30, 90) * 600);
        self.transition(PetState::Idle, Some(dur));
    }

    fn enter_walk(&mut self) {
        // 20~60 tick × 180ms
        let dur = Duration::from_millis(self.rand_range(20, 60) * 180);
        self.transition(PetState::Walk, Some(dur));
        self.dir = if self.rand() % 2 == 0 { 1 } else { -1 };
    }

    fn enter_pose(&mut self, state: PetState, dur: Option<Duration>, msg: Option<&str>) {
        self.transition(state, dur);
        self.frame_at = None;
        if let Some(m) = msg {
            self.bubble_show(m);
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn enter_climb(&mut self, wall: i32) {
        self.transition(PetState::Climb, None); // 爬墙不判时长
        self.climb_wall = wall;
        self.climb_vertical = -1;
        self.timers.cancel(Deadline::Hang);
        self.bubble_show("爬墙咯！");
    }

    fn enter_sleep_by_bubble(&mut self, msg: &str) {
        self.bubble_show(msg);
        self.transition(PetState::Sleep, None); // 睡觉不自动结束
        self.frame_at = None;
        self.timers.cancel(Deadline::ReactionEnd);
        self.timers.cancel(Deadline::Hang);
    }

    fn wake_with(&mut self, msg: &str) {
        self.enter_idle();
        self.timers.cancel(Deadline::ReactionEnd);
        self.timers.cancel(Deadline::Hang);
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
                let (cx, cy) = (self.pos.0 + self.pet_size / 2, self.pos.1 + self.pet_size / 2);
                let (dx, dy) = (mx - cx, my - cy);
                let gx = if dx > 28 { 1 } else if dx < -28 { -1 } else { 0 };
                let gy = if dy > 28 { 1 } else if dy < -28 { -1 } else { 0 };
                return (gx, gy);
            }
        }
        (0, 0)
    }

    /// 状态机推进：分发到各状态的子方法；末尾统一处理气泡跟随。
    fn advance(&mut self, window: &Window) {
        // 右键菜单打开期间暂停位移（菜单不跟着跑），动画帧照常
        let ui_modal = self.menu.is_some();
        // 跟随鼠标模式：朝光标水平位置走，靠近后坐下看（输入框打开时不追，安静陪着打字）
        if self.settings.follow_mouse
            && self.input.is_none()
            && matches!(self.state, PetState::Idle | PetState::Walk)
        {
            #[cfg(windows)]
            if let Some((mx, _)) = cursor_pos() {
                let target = mx - self.pet_size / 2;
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
            PetState::Idle => self.advance_idle(),
            PetState::Walk => self.advance_walk(window, ui_modal),
            PetState::Sitting
            | PetState::Stretch
            | PetState::Groom
            | PetState::Eat
            | PetState::Roll
            | PetState::PlayBall
            | PetState::TailChase
            | PetState::Wave
            | PetState::Purr
            | PetState::Startle => {
                self.tick += 1;
                if self.state_until.map(|t| Instant::now() >= t).unwrap_or(false) {
                    self.enter_idle();
                }
            }
            PetState::Perch => self.advance_perch(window, ui_modal),
            PetState::Climb => self.advance_climb(window, ui_modal),
            PetState::Thrown => self.advance_thrown(window, ui_modal),
            PetState::Dragged | PetState::Patted | PetState::Shocked | PetState::Sleep => {
                self.tick += 1;
            }
        }
        // 气泡跟随宠物移动（散步/爬墙/飞出/趴窗跟随），不再留在原地指向旧位置
        if self.timers.is_set(Deadline::BubbleHide) || self.chat_pending {
            let (pp, ps, mon, above) = (self.pos, self.pet_size, self.mon, self.bubble_above());
            if let Some(b) = &mut self.bubble {
                if b.is_visible() {
                    b.reposition(pp, ps, mon, above);
                }
            }
        }
    }

    /// 待机：到期按睡意入睡或去散步（输入框打开时原地陪打字，睡意不累积）
    fn advance_idle(&mut self) {
        self.tick += 1;
        if self.state_until.map(|t| Instant::now() >= t).unwrap_or(false) {
            if self.idle_cycles >= 2 {
                self.transition(PetState::Sleep, None);
            } else if self.input.is_none() {
                self.idle_cycles += 1;
                self.enter_walk();
            } else {
                self.tick = 0;
            }
        }
    }

    /// 散步：位移 + 撞墙概率爬墙，到期随机切换休息姿势
    fn advance_walk(&mut self, window: &Window, ui_modal: bool) {
        self.tick += 1;
        if !ui_modal {
            self.pos.0 += self.dir * self.walk_step();
            if self.pos.0 < self.mon.x {
                self.pos.0 = self.mon.x;
                self.dir = 1;
                // 撞左墙：概率爬墙
                if self.mon.h >= 240 && self.rand() % 10 < 4 {
                    self.pos.0 += self.walk_step();
                    self.enter_climb(-1);
                }
            } else if self.pos.0 > self.mon.x + self.mon.w - self.pet_size {
                self.pos.0 = self.mon.x + self.mon.w - self.pet_size;
                self.dir = -1;
                if self.mon.h >= 240 && self.rand() % 10 < 4 {
                    self.pos.0 -= self.walk_step();
                    self.enter_climb(1);
                }
            }
            window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
        }
        if self.state_until.map(|t| Instant::now() >= t).unwrap_or(false) {
            // 随机休息姿势：坐 / 伸懒腰 / 舔毛，小概率去趴窗（时长与旧"帧数×帧间隔"等价）
            let roll = self.rand() % 100;
            if roll <= 9 && self.mon.h >= 240 {
                self.perch_on_window();
            } else if roll <= 34 {
                let dur = Duration::from_millis(self.rand_range(6, 14) * 600);
                self.enter_pose(PetState::Sitting, Some(dur), None);
            } else if roll <= 49 {
                self.enter_pose(PetState::Stretch, Some(Duration::from_millis(1600)), Some("伸个懒腰～"));
            } else if roll <= 69 {
                let dur = Duration::from_millis(self.rand_range(8, 16) * 600);
                self.enter_pose(PetState::Groom, Some(dur), None);
            } else {
                self.enter_idle();
            }
        }
    }

    /// 趴在窗口顶边：跟随目标窗口，到期（或窗口没了）跳下
    fn advance_perch(&mut self, window: &Window, ui_modal: bool) {
        self.tick += 1;
        #[cfg(windows)]
        if !ui_modal {
            self.perch_follow();
        }
        let done = self.perch.as_ref().map(|(_, _, t)| *t == 0).unwrap_or(true);
        if done {
            // 从窗口上跳下来
            self.perch = None;
            self.transition(PetState::Thrown, None);
            self.thrown_vel = (0.0, -0.1);
            self.frame_at = Some(Instant::now() + Duration::from_millis(THROWN_FRAME_MS));
            return;
        }
        if !ui_modal {
            window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
        }
    }

    /// 爬屏幕边缘：上行 → 顶上挂一会儿 → 下行落地回屏幕内
    fn advance_climb(&mut self, window: &Window, ui_modal: bool) {
        self.tick += 1;
        if self.timers.is_set(Deadline::Hang) {
            // 在顶上挂一会儿（到期由 Deadline::Hang fire 解除）
        } else if self.climb_vertical < 0 {
            self.pos.1 -= self.climb_step();
            if self.pos.1 <= self.mon.y + 2 {
                self.pos.1 = self.mon.y + 2;
                self.timers.set(Deadline::Hang, Instant::now() + Duration::from_millis(1200));
            }
        } else {
            self.pos.1 += self.climb_step();
            if self.pos.1 >= self.mon_bottom() {
                self.pos.1 = self.mon_bottom();
                // 落地后往屏幕里挪，结束爬墙
                self.pos.0 = if self.climb_wall > 0 {
                    self.pos.0 - 12
                } else {
                    self.pos.0 + 12
                };
                self.climb_wall = 0;
                self.timers.cancel(Deadline::Hang);
                self.enter_idle();
            }
        }
        if !ui_modal {
            window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
        }
    }

    /// 被甩飞：抛物线 + 弹跳衰减，落稳后晕一下
    fn advance_thrown(&mut self, window: &Window, ui_modal: bool) {
        let dt = THROWN_FRAME_MS as f32;
        // 重力按体型比例：512 大猫与 64 小猫的抛物线手感一致
        let gravity = GRAVITY * (self.pet_size as f32 / 64.0);
        let (mut vx, mut vy) = self.thrown_vel;
        let mut landed = false;
        if !ui_modal {
            self.pos.0 += (vx * dt) as i32;
            self.pos.1 += (vy * dt) as i32;
            vy += gravity * dt;
            if self.pos.0 < self.mon.x {
                self.pos.0 = self.mon.x;
                vx = -vx * 0.7;
            }
            if self.pos.0 > self.mon.x + self.mon.w - self.pet_size {
                self.pos.0 = self.mon.x + self.mon.w - self.pet_size;
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
        }
        if landed {
            self.resolve_mon();
            self.enter_idle();
            self.timers.set(Deadline::ReactionEnd, Instant::now() + Duration::from_millis(700));
            self.transition(PetState::Shocked, None);
            self.bubble_show("喵呜…晕了");
        }
    }

    fn frame_duration(&self) -> Duration {
        // 帧序列模型用自己的 fps，内置模型按状态节拍
        Duration::from_millis(self.model.frame_ms(&self.state))
    }

    fn resolve_mon(&mut self) {
        // 容差 = 边长 1/4：贴边落点不突跳到另一块屏
        let tol = (self.pet_size / 4).max(1);
        if let Some(m) = self
            .mons
            .iter()
            .find(|m| m.contains_center_tol(self.pos.0, self.pos.1, self.pet_size, tol))
        {
            if m.x != self.mon.x || m.y != self.mon.y {
                self.mon = *m;
            }
        }
    }

    // ---------- 定时器 ----------

    fn next_wakeup(&self) -> Option<Instant> {
        let now = Instant::now();
        let press_deadline = self.press.map(|p| p.start + Duration::from_millis(DRAG_START_MS));
        let typing_tick = self.bubble.as_ref().and_then(|b| b.next_tick_at());
        [
            self.frame_at,
            self.typing_until,
            typing_tick,
            press_deadline,
        ]
        .into_iter()
        .flatten()
        .chain(self.timers.iter_times())
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

    /// 气泡放头顶还是脚下：宠物贴近屏幕上缘时放脚下
    fn bubble_above(&self) -> bool {
        self.pos.1 > self.mon.y + deskpet::ui(90)
    }

    fn bubble_show(&mut self, text: &str) {
        if self.hidden {
            return; // 全屏遮挡期间绝不弹泡（游戏/视频零干扰）
        }
        let secs = (2 + text.chars().count() as u64 / 6).min(8);
        let above = self.bubble_above();
        if let Some(b) = &mut self.bubble {
            let hide_at = Instant::now();
            if self.settings.typewriter {
                b.show(text, self.pos, self.pet_size, self.mon, above);
                // 显示时长要覆盖打字过程
                let typing = b.typing_remaining().as_secs() as u64 + 1;
                self.timers
                    .set(Deadline::BubbleHide, hide_at + Duration::from_secs(secs.max(typing + 2)));
            } else {
                b.show_now(text, self.pos, self.pet_size, self.mon, above);
                self.timers.set(Deadline::BubbleHide, hide_at + Duration::from_secs(secs));
            }
        }
    }

    /// 思考中动画泡：不设过期时间，回复到达时替换（即时显示，不走打字机）
    fn bubble_show_persistent(&mut self, text: &str) {
        if self.hidden {
            return;
        }
        let above = self.bubble_above();
        if let Some(b) = &mut self.bubble {
            b.show_now(text, self.pos, self.pet_size, self.mon, above);
        }
        self.timers.cancel(Deadline::BubbleHide);
    }

    fn bubble_hide(&mut self) {
        self.timers.cancel(Deadline::BubbleHide);
        if let Some(b) = &mut self.bubble {
            b.hide();
        }
    }

    // ---------- 交互 ----------

    fn on_left_press(&mut self, window: &Window) {
        let now = Instant::now();
        // 双击判定锚在上一次"短点击"的松开时刻：单击摸头（~150ms 松开）后
        // 立刻按住想拖动，不会落进 400ms 窗口被误判成双击睡觉
        if let Some((t, p)) = self.last_release {
            if is_double_click(now, (t, p), self.cursor) {
                self.last_release = None;
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
        self.press_cursor = self.cursor;

        if self.state == PetState::Sleep {
            self.wake_with("喵呜…我醒啦");
            window.request_redraw();
            return;
        }
        self.press = Some(PressInfo { start: now, moved: false });
    }

    fn begin_drag(&mut self) {
        self.transition(PetState::Dragged, None);
        self.drag_origin = self.pos;
        // 按下时窗口未动：本地坐标 == 全局抓取偏移，拖动全程锚定它
        self.grab = self.cursor;
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
            // 拖动/甩飞后的按下不是双击的前半段
            self.last_release = None;
            let (vx, vy) = self.drag_velocity();
            let speed = (vx * vx + vy * vy).sqrt();
            if speed > FLING_SPEED_MIN {
                // 甩飞！
                self.transition(PetState::Thrown, None);
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
            // 阈值按体型比例（64px 猫 = 24px）
            if moved_px > self.pet_size * 3 / 8 {
                self.bubble_show("呼…安全着陆！");
            }
            self.save_session();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }
        if let Some(p) = press {
            if p.start.elapsed() < Duration::from_millis(CLICK_MAX_MS) && !p.moved {
                // 记为一次短点击（双击判定的锚点）
                self.last_release = Some((Instant::now(), self.cursor));
                self.click_zone();
            }
        }
    }

    fn click_zone(&mut self) {
        let y = self.cursor.1 as i32;
        // 分区按体型比例：上 1/3 摸头、中 1/3 戳肚子、下 1/3 踩脚
        let (msg, dur) = if y < self.pet_size / 3 {
            ("好舒服～再摸摸！", 1400u64)
        } else if y < self.pet_size * 2 / 3 {
            ("呜哇！别戳肚子啦！", 1100)
        } else {
            ("喵嗷！踩到脚了！", 900)
        };
        self.transition(if y < 20 { PetState::Patted } else { PetState::Shocked }, None);
        self.timers.set(Deadline::ReactionEnd, Instant::now() + Duration::from_millis(dur));
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
        // 10 tick × 200ms = 2s
        self.enter_pose(PetState::Eat, Some(Duration::from_millis(2000)), Some("咔嚓咔嚓…小鱼干最棒了！"));
        let cfg = self.cfg.clone();
        tts::speak(&self.client, &cfg, self.settings.voice, "小鱼干最棒了喵！");
    }

    fn whisper(&mut self) {
        if self.settings.whisper_on
            && !self.settings.quiet
            && self.state == PetState::Idle
            && !self.timers.is_set(Deadline::BubbleHide)
            && !self.chat_pending
            && !self.hidden
        {
            let i = (self.rand() % WHISPERS.len() as u64) as usize;
            self.bubble_show(WHISPERS[i]);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        let next = Instant::now() + Duration::from_secs(self.rand_range(50, 140));
        self.timers.set(Deadline::Whisper, next);
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
        match surface.buffer_mut() {
            Ok(mut buf) => {
                for (dst, src) in buf.iter_mut().zip(sprite.iter()) {
                    // 透明像素填色键（分层窗口：视觉透明 + 点击穿透）
                    *dst = if src & 0xFF000000 == 0 {
                        COLORKEY
                    } else {
                        0xFF000000 | (src & 0x00FFFFFF)
                    };
                }
                if let Err(_) = buf.present() {
                    dwarn("present", "宠物窗口呈现失败（GDI 异常？）");
                }
            }
            Err(_) => {
                dwarn("present", "渲染缓冲获取失败，跳过本帧");
            }
        }
        self.frame_at = Some(Instant::now() + self.frame_duration());
        self.schedule(el);
    }

    // ---------- 窗口 ----------

    /// 打开/重新贴位悬浮输入框
    fn ensure_input(&mut self, el: &ActiveEventLoop) {
        if let Some(i) = &mut self.input {
            i.open(self.pos, self.pet_size, self.mon);
            return;
        }
        let ph = format!("和{}说点什么…", self.cfg.pet_name);
        if let Some(mut i) = InputBox::create(el, ph) {
            i.open(self.pos, self.pet_size, self.mon);
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

    /// 发送一条消息（文本或拖入的图片）：回复以气泡 + 语音呈现。
    /// 上一条还没回复时不并发请求（回复会乱序、思考动画错乱），
    /// 被拒的文本放回输入框不丢字。
    fn send_chat(&mut self, text: String, image: Option<String>) {
        dlog(&format!(
            "聊天发送：{}{}",
            if self.cfg.ai_ready() { "（AI）" } else { "（离线）" },
            text.chars().take(20).collect::<String>()
        ));
        if self.chat_pending {
            self.bubble_show("等我说完这句嘛～");
            if let Some(i) = &mut self.input {
                if image.is_none() {
                    i.input = text;
                }
                i.clear_pending();
            }
            return;
        }
        self.chat_history
            .push(ChatMsg { role: Role::User, text, image: image.clone() });
        self.trim_history();
        self.log_chat("user", &self.chat_history.last().map(|m| m.text.clone()).unwrap_or_default());
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
        self.timers.set_recur(Deadline::Think, Instant::now() + Duration::from_millis(350), Duration::from_millis(350));
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
                std::thread::spawn(move || {
                    // 最多 3 次尝试（间隔 1s/2s）：仅网络/限流/5xx 重试；
                    // 密钥错误/模型不存在重试也不会好，直接给出可读文案
                    let mut result: Result<String, ChatErr> = Ok(String::new());
                    let mut ok = false;
                    let mut attempts: u32 = 0;
                    let mut last_err: Option<ChatErr> = None;
                    for attempt in 0u32..3 {
                        attempts = attempt + 1;
                        if attempt > 0 {
                            std::thread::sleep(Duration::from_secs(if attempt == 1 { 1 } else { 2 }));
                        }
                        match client.chat(&cfg, &model, &h) {
                            Ok(r) => {
                                result = Ok(r);
                                ok = true;
                                break;
                            }
                            Err(e) => {
                                let retriable = e.retryable();
                                last_err = Some(e);
                                if !retriable {
                                    break;
                                }
                            }
                        }
                    }
                    let reply = if ok {
                        result.unwrap_or_default()
                    } else {
                        let mut msg = last_err.map(|e| e.message()).unwrap_or_default();
                        if attempts > 1 {
                            msg.push_str(&format!("（重试 {} 次仍失败）", attempts - 1));
                        }
                        msg
                    };
                    let _ = proxy.send_event(PetEvent::ChatReply(Ok(reply)));
                });
            }
            None => {
                self.chat_pending = false;
                self.timers.cancel(Deadline::Think);
                self.chat_history
                    .push(ChatMsg { role: Role::Pet, text: "还没接入 AI".into(), image: None });
                self.trim_history();
                // 文案与自动生成的模板一致（模板已自动生成，不存在 .example）
                const OFFLINE_HINT: &str = "打开我旁边的 deskpet.toml，取消 base_url 和 api_key 两行注释并填入密钥，重启就能聊天啦";
                self.bubble_show(&format!("喵呜～我还没接入 AI！{OFFLINE_HINT}"));
                if let Some(i) = &mut self.input {
                    i.clear_pending();
                    // 气泡 8 秒就没了，输入框里留一条更持久的引导
                    i.input = format!("未接入 AI：{OFFLINE_HINT}");
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
        self.refresh_model_registry(); // models/ 新增的模型免重启出现
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

    /// 切换到注册表第 i 个模型
    fn switch_model(&mut self, i: usize) {
        let Some((name, source)) = self.models.get(i).map(|(n, s)| (n.clone(), s.clone())) else {
            return;
        };
        match source {
            ModelSource::Builtin(kind) => {
                self.model_kind = i;
                self.model = make_model(kind);
            }
            ModelSource::Sprite(path) => match SpriteModel::load(path) {
                Some(m) => {
                    self.model = Box::new(m);
                    self.model_kind = i;
                }
                None => {
                    dwarn("model-load", &format!("模型 {} 加载失败", name));
                    self.bubble_show("这个模型加载失败了喵");
                    return;
                }
            },
        }
        // 同步窗口与 surface 尺寸（模型间尺寸不同：猫 512、其他 64）
        let side = self.model.size().0 as i32;
        if side != self.pet_size {
            self.pet_size = side;
            if let Some(w) = &self.window {
                let _ = w.request_inner_size(PhysicalSize::new(side as u32, side as u32));
            }
            if let Some(surface) = &mut self.surface {
                let _ = surface.resize(
                    std::num::NonZeroU32::new(side as u32).unwrap(),
                    std::num::NonZeroU32::new(side as u32).unwrap(),
                );
            }
            // 位置夹回屏幕（大猫换小猫时防止悬在屏外）
            self.pos.0 = self.pos.0.min(self.mon.x + self.mon.w - side).max(self.mon.x);
            self.pos.1 = self.pos.1.min(self.mon_bottom()).max(self.mon.y);
            if let Some(w) = &self.window {
                w.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
            }
        }
        self.bubble_show(&format!("嗨！我是{}～", name));
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn root_entries(&self) -> Vec<Entry> {
        let onoff = |on: bool| if on { "开" } else { "关" };
        vec![
            Entry::item("pet-chat", "对话"),
            Entry::item("pet-todo", "待办清单"),
            Entry::sep(),
            Entry::sub(Page::Fun, "▸ 互动"),
            Entry::sub(Page::Model, "▸ 模型"),
            Entry::sub(Page::Costume, "▸ 换装"),
            Entry::sub(Page::Expr, "▸ 表情"),
            Entry::sub(Page::Set, "▸ 设置"),
            Entry::sep(),
            Entry::stay("pet-remind", format!("提醒：{}", onoff(self.settings.drink_minutes > 0))),
            Entry::stay("set-voice", format!("语音播报：{}", onoff(self.settings.voice))),
            Entry::stay("set-quiet", format!("勿扰模式：{}", onoff(self.settings.quiet))),
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
            Entry::sep(),
            Entry::item("act-roll", "打个滚"),
            Entry::item("act-ball", "玩球球"),
            Entry::item("act-tail", "追尾巴"),
            Entry::item("act-wave", "招招手"),
            Entry::item("act-purr", "呼噜噜"),
            Entry::item("act-startle", "炸个毛"),
            Entry::sep(),
            Entry::item("pet-perch", "趴到窗口上"),
            Entry::item("pet-throw", "扔个窗口"),
            Entry::stay("pet-say", "说句话"),
        ]
    }

    /// 刷新模型注册表：内置物种 + models/ 目录下的帧序列模型
    fn refresh_model_registry(&mut self) {
        self.models.clear();
        for (i, name) in model_names().into_iter().enumerate() {
            self.models.push((name, ModelSource::Builtin(i)));
        }
        let dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("models")))
            .unwrap_or_else(|| std::path::PathBuf::from("models"));
        for (name, path) in deskpet::sprite_model::discover(&dir) {
            self.models.push((name, ModelSource::Sprite(path)));
        }
    }

    fn model_entries(&self) -> Vec<Entry> {
        let cur = self.model_kind;
        let mut v: Vec<Entry> = self
            .models
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                Entry::stay(
                    &format!("model-{i}"),
                    format!("{} {}", if i == cur { "●" } else { "○" }, name),
                )
            })
            .collect();
        v.push(Entry::back());
        v
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
            Entry::stay("set-hotkeys", format!("全局快捷键：{}", onoff(s.hotkeys))),
            Entry::stay("set-typewriter", format!("打字机气泡：{}", onoff(s.typewriter))),
            Entry::stay("set-chatlog", format!("聊天记录落盘：{}", onoff(s.chat_log))),
            Entry::stay("set-drink", format!("喝水提醒：{}", onoff(s.drink_minutes > 0))),
            Entry::stay("set-sit", format!("久坐提醒：{}", onoff(s.sit_minutes > 0))),
            Entry::sep(),
            Entry::stay("set-tts-sample", "试听音色"),
            Entry::item("pet-settings-help", "AI 参数请编辑 deskpet.toml"),
        ]
    }

    fn menu_action(&mut self, id: &str, el: &ActiveEventLoop) {
        match id {
            "quit" | "tray-quit" => {
                self.save_session();
                el.exit();
            }
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
            "set-quiet" => {
                self.settings.quiet = !self.settings.quiet;
                self.bubble_show(if self.settings.quiet {
                    "勿扰模式开启：不主动说话不出声喵"
                } else {
                    "勿扰模式关闭，恢复碎碎念和提醒"
                });
                self.persist_toggles();
            }
            "set-hotkeys" => {
                self.settings.hotkeys = !self.settings.hotkeys;
                self.bubble_show(if self.settings.hotkeys {
                    "全局快捷键开启：Ctrl+Shift + D勿扰/T待办/C聊天/H隐藏/Q退出"
                } else {
                    "全局快捷键关闭"
                });
                self.persist_toggles();
            }
            "set-typewriter" => {
                self.settings.typewriter = !self.settings.typewriter;
                self.bubble_show(if self.settings.typewriter {
                    "打字机气泡开启啦"
                } else {
                    "打字机气泡关闭"
                });
                self.persist_toggles();
            }
            "set-chatlog" => {
                self.settings.chat_log = !self.settings.chat_log;
                self.bubble_show(if self.settings.chat_log {
                    "聊天记录会存到 chat_log.jsonl（exe 旁边）"
                } else {
                    "聊天记录落盘已关闭"
                });
                self.persist_toggles();
            }
            "set-tts-sample" => {
                if self.settings.voice {
                    let cfg = self.cfg.clone();
                    tts::speak_force(&self.client, &cfg, true, "你好，我是团子，这是我的声音喵！");
                } else {
                    self.bubble_show("语音总开关是关的，先在根页打开语音播报");
                }
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
                let mem = working_set_mb();
                let mem_line = mem.map(|m| format!("内存 {m:.1} MB · ")).unwrap_or_default();
                self.bubble_show(&format!(
                    "{} v0.3.2 · 纯Rust桌宠\n{mem_line}github.com/summerliuguang/deskpet\n右键→设置 可开关功能",
                    self.model.info().name
                ));
            }
            "act-roll" => self.enter_pose(PetState::Roll, Some(Duration::from_millis(700)), Some("打个滚～")),
            "act-ball" => self.enter_pose(PetState::PlayBall, Some(Duration::from_millis(800)), Some("球球是我的！")),
            "act-tail" => self.enter_pose(PetState::TailChase, Some(Duration::from_millis(800)), Some("尾巴抓不住！")),
            "act-wave" => self.enter_pose(PetState::Wave, Some(Duration::from_millis(600)), Some("嗨～")),
            "act-purr" => self.enter_pose(PetState::Purr, Some(Duration::from_millis(900)), Some("呼噜呼噜…")),
            "act-startle" => self.enter_pose(PetState::Startle, Some(Duration::from_millis(600)), Some("喵嗷！！")),
            "pet-perch" => self.perch_on_window(),
            other if other.starts_with("model-") => {
                if let Ok(i) = other.strip_prefix("model-").unwrap().parse::<usize>() {
                    if i != self.model_kind {
                        self.switch_model(i);
                    }
                }
            }
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

    /// 退出/换模型/拖动结束时的会话保存：开关 + 位置 + 模型选择
    fn save_session(&mut self) {
        self.persist_toggles();
        let mut pairs: Vec<(String, toml::Value)> = Vec::new();
        if !self.hidden {
            pairs.push(("pos_x".into(), toml::Value::Integer(self.pos.0 as i64)));
            pairs.push(("pos_y".into(), toml::Value::Integer(self.pos.1 as i64)));
        }
        pairs.push(("model_kind".into(), toml::Value::Integer(self.model_kind as i64)));
        if let Some((name, _)) = self.models.get(self.model_kind) {
            pairs.push(("model_name".into(), toml::Value::String(name.clone())));
        }
        deskpet::config::persist_settings(&pairs);
    }

    fn persist_toggles(&self) {
        // 开关清单由 for_each_setting 宏维护（与 Default/parse 同源；
        // opt 字段不持久化——pos/model_kind 由 save_session 按语义单独写）
        // persist_one 定义在 persist_fields 宏体之外：嵌套定义时内层规则头的
        // $d 会被外层宏解析器误认为自身重复变量的引用（still repeating at this depth）
        macro_rules! persist_one {
            ($pairs:ident, $st:ident, $n:ident, bool, $d:tt) => {
                $pairs.push((stringify!($n).into(), toml::Value::Boolean($st.$n)));
            };
            ($pairs:ident, $st:ident, $n:ident, u64, $d:tt) => {
                $pairs.push((stringify!($n).into(), toml::Value::Integer($st.$n as i64)));
            };
            ($pairs:ident, $st:ident, $n:ident, opt_i64, none) => {};
            ($pairs:ident, $st:ident, $n:ident, opt_usize, none) => {};
            ($pairs:ident, $st:ident, $n:ident, opt_string, none) => {};
        }
        macro_rules! persist_fields {
            ({ $(($name:ident, $ty:ident, $d:tt))* }) => {{
                let st = &self.settings;
                #[allow(unused_mut)]
                let mut pairs: Vec<(String, toml::Value)> = Vec::new();
                $( persist_one!(pairs, st, $name, $ty, $d); )*
                deskpet::config::persist_settings(&pairs);
            }};
        }
        for_each_setting!(persist_fields);
    }

    // ---------- 提醒 ----------

    fn arm_reminders(&mut self) {
        let now = Instant::now();
        if self.settings.drink_minutes > 0 {
            self.timers
                .set(Deadline::Drink, now + Duration::from_secs(self.settings.drink_minutes * 60));
        } else {
            self.timers.cancel(Deadline::Drink);
        }
        if self.settings.sit_minutes > 0 {
            self.timers
                .set(Deadline::Sit, now + Duration::from_secs(self.settings.sit_minutes * 60));
        } else {
            self.timers.cancel(Deadline::Sit);
        }
    }

    fn fire_reminder(&mut self, drink: bool) {
        let (msg, tts_text) = if drink {
            ("叮咚——该喝水啦！", "主人，该喝水啦")
        } else {
            ("坐好久啦，起来伸个懒腰喵！", "主人，坐久了，起来活动一下吧")
        };
        if self.hidden || self.settings.quiet {
            // 全屏遮挡/勿扰：静默顺延，不弹泡不出声
            self.arm_reminders();
            return;
        }
        if self.state != PetState::Sleep {
            self.transition(PetState::Patted, None);
            self.timers.set(Deadline::ReactionEnd, Instant::now() + Duration::from_millis(1500));
        }
        self.bubble_show(msg);
        let cfg = self.cfg.clone();
        tts::speak(&self.client, &cfg, self.settings.voice, tts_text);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        self.arm_reminders();
    }

    // ---------- 隐藏/恢复（全屏遮挡与快捷键共用） ----------

    /// 显示器热插拔/分辨率变化：重枚举，宠物所在屏消失时迁移并夹回可见区域
    fn refresh_monitors(&mut self, el: &ActiveEventLoop) {
        let new_mons = collect_mons(el);
        if new_mons.is_empty() || new_mons == self.mons {
            return;
        }
        self.mons = new_mons;
        dlog(&format!("显示器变化 → {} 块", self.mons.len()));
        let still_here = self
            .mons
            .iter()
            .any(|m| m.x == self.mon.x && m.y == self.mon.y);
        if !still_here {
            // 原屏没了：找包含宠物中心的屏，否则搬到第一块
            let cx = self.pos.0 + self.pet_size / 2;
            let cy = self.pos.1 + self.pet_size / 2;
            self.mon = self
                .mons
                .iter()
                .find(|m| m.contains_point(cx, cy))
                .copied()
                .unwrap_or(self.mons[0]);
        }
        // 夹回当前屏可见范围
        let max_x = (self.mon.x + self.mon.w - self.pet_size).max(self.mon.x);
        let max_y = self.mon_bottom().max(self.mon.y);
        let new_pos = (
            self.pos.0.clamp(self.mon.x, max_x),
            self.pos.1.clamp(self.mon.y, max_y),
        );
        if new_pos != self.pos {
            self.pos = new_pos;
            if let Some(w) = &self.window {
                w.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 事件处理 panic 兜底：记录日志并恢复动画调度，宠物不闪退
    /// （release 为 unwind 策略，工作线程 panic 也只死线程不死进程）
    fn recover_from_panic(&mut self, stage: &str) {
        dwarn(
            &format!("panic-{stage}"),
            &format!("事件处理 panic 已拦截（{stage}），宠物继续运行"),
        );
        self.press = None;
        self.frame_at = Some(Instant::now() + Duration::from_millis(300));
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 冒烟模式：记录某窗口已完成一次渲染
    fn mark_selftest_drawn(&mut self, which: &'static str) {
        if self.selftest && !self.selftest_seen.contains(&which) {
            self.selftest_seen.push(which);
        }
    }

    fn window_event_inner(&mut self, el: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        // 气泡窗口重绘路由（尺寸生效后的修正帧）
        if self.bubble.as_ref().is_some_and(|b| b.window.id() == window_id) {
            if matches!(event, WindowEvent::RedrawRequested) {
                if let Some(b) = &mut self.bubble {
                    b.redraw();
                }
            }
            return;
        }
        // 右键菜单窗口事件路由
        if self.menu.as_ref().is_some_and(|m| m.window.id() == window_id) {
            if matches!(event, WindowEvent::RedrawRequested) {
                self.mark_selftest_drawn("menu");
            }
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
                        // 先关菜单（销毁窗口）再执行动作：动作里打开的输入框
                        // 才不会被随后的菜单窗口销毁抢走焦点
                        if !stay {
                            self.menu = None;
                        }
                        self.menu_action(&id, el);
                        if stay {
                            // stay 类动作（换装/表情/开关）处理后刷新标签，菜单保持打开
                            let mut menu = self.menu.take().unwrap();
                            let entries = match menu.page() {
                                Page::Root => self.root_entries(),
                                Page::Model => self.model_entries(),
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
        }

        // 悬浮输入框事件路由
        if self.input.as_ref().is_some_and(|i| i.window.id() == window_id) {
            if matches!(event, WindowEvent::RedrawRequested) {
                self.mark_selftest_drawn("input");
            }
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
                    InputAction::Send { text, image } => {
                        if image.is_none() {
                            if let Some(i) = &mut self.input {
                                i.push_history(text.clone());
                            }
                        }
                        self.send_chat(text, image);
                    }
                }
                    return;
                }
            }
        }
        // 待办窗事件路由
        if self.todo.as_ref().is_some_and(|t| t.window.id() == window_id) {
            if matches!(event, WindowEvent::RedrawRequested) {
                self.mark_selftest_drawn("todo");
            }
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
        }

        let Some(window) = self.window.clone() else { return };
        if window.id() != window_id {
            return;
        }
        match event {
            WindowEvent::RedrawRequested => {
                self.mark_selftest_drawn("pet");
                self.draw_pet(el);
            }
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
                            // 锚定全局：CursorMoved 的本地坐标 + 事件时窗口位置
                            // （self.pos 尚未更新，正是事件时刻的位置）= 鼠标全局坐标。
                            // 旧实现拿本地坐标直接减抓取偏移，但窗口一动本地坐标系
                            // 随之平移，稳态下窗口只有鼠标一半速度，越拖掉队越远。
                            let nx = apply_drag(self.pos, self.grab, self.cursor);
                            // 允许跨屏拖动：夹到所有显示器的联合包围盒
                            let (min_x, min_y) = self
                                .mons
                                .iter()
                                .fold((i32::MAX, i32::MAX), |a, m| (a.0.min(m.x), a.1.min(m.y)));
                            let (max_x, max_y) = self.mons.iter().fold(
                                (i32::MIN, i32::MIN),
                                |a, m| (a.0.max(m.x + m.w - self.pet_size), a.1.max(m.y + m.h - self.pet_size)),
                            );
                            self.pos = (nx.0.clamp(min_x, max_x), nx.1.clamp(min_y, max_y));
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
            WindowEvent::ScaleFactorChanged { scale_factor, mut inner_size_writer } => {
                // 拖到不同缩放的显示器：按新缩放重设宠物窗口
                deskpet::set_ui_scale(scale_factor);
                // 内置像素模型的 sprite_scale 在构造时按当时的 ui_scale 定死，
                // 缩放变化必须重建（保留换装/表情）；帧序列模型 64px 固定不受影响
                if let Some((_, ModelSource::Builtin(kind))) = self.models.get(self.model_kind).map(|(n, s)| (n.clone(), s.clone())) {
                    let (costume, expr) = (self.model.costume(), self.model.expression());
                    self.model = make_model(kind);
                    self.model.set_costume(costume);
                    self.model.set_expression(expr);
                }
                self.pet_size = self.model.size().0 as i32;
                let _ = inner_size_writer.request_inner_size(PhysicalSize::new(
                    self.pet_size as u32,
                    self.pet_size as u32,
                ));
                if let Some(surface) = &mut self.surface {
                    let _ = surface.resize(
                        NonZeroU32::new(self.pet_size as u32).unwrap(),
                        NonZeroU32::new(self.pet_size as u32).unwrap(),
                    );
                }
                // 派生 UI 关闭/重建，重开时按新缩放
                self.rebuild_derived_for_scale(el);
                window.request_redraw();
                dlog(&format!("缩放变化 → {scale_factor:.2}"));
            }
            WindowEvent::Ime(Ime::Enabled) | WindowEvent::Ime(Ime::Disabled) => {}
            WindowEvent::CloseRequested => {
                self.save_session();
                el.exit();
            }
            _ => {}
        }
    }

    fn user_event_inner(&mut self, el: &ActiveEventLoop, event: PetEvent) {
        match event {
            PetEvent::FullscreenChanged(fs) => self.set_hidden(el, fs),
            PetEvent::MonitorsChanged => {
                self.refresh_monitors(el);
            }
            PetEvent::Hotkey(id) => {
                if !self.settings.hotkeys {
                    return;
                }
                match id {
                    0 => self.menu_action("set-quiet", el),
                    1 => self.ensure_todo(el),
                    2 => self.ensure_input(el),
                    3 => self.set_hidden(el, !self.hidden),
                    4 => {
                        self.save_session();
                        el.exit();
                    }
                    _ => {}
                }
            }
            PetEvent::ChatReply(r) => {
                self.chat_pending = false;
                self.timers.cancel(Deadline::Think);
                let reply = match r {
                    Ok(t) => t,
                    Err(e) => format!("出错了：{e}"),
                };
                // 全屏/勿扰时不出声（游戏/视频不被打断；文字气泡照常）
                if !self.hidden && !self.settings.quiet {
                    let cfg = self.cfg.clone();
                    tts::speak(&self.client, &cfg, self.settings.voice, &reply);
                }
                self.chat_history
                    .push(ChatMsg { role: Role::Pet, text: reply.clone(), image: None });
                self.trim_history();
                self.log_chat("pet", &reply);
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
                    self.transition(PetState::Patted, None);
                    self.timers.set(Deadline::ReactionEnd, Instant::now() + Duration::from_millis(800));
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

    /// 统一定时器分发：每种 Deadline 的到期动作（逻辑自原 about_to_wait 各段原样迁移）
    fn fire_deadline(&mut self, _el: &ActiveEventLoop, kind: Deadline) {
        match kind {
            Deadline::BubbleHide => self.bubble_hide(),
            Deadline::ReactionEnd => {
                if self.state == PetState::Patted || self.state == PetState::Shocked {
                    self.enter_idle();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            Deadline::Whisper => self.whisper(),
            Deadline::Hang => {
                if self.state == PetState::Climb {
                    self.climb_vertical = 1;
                    self.frame_at = Some(Instant::now() + Duration::from_millis(180));
                }
            }
            Deadline::Drink => self.fire_reminder(true),
            Deadline::Sit => self.fire_reminder(false),
            Deadline::Autosave => self.save_session(),
            Deadline::DueCheck => {
                // 清单开着时查到期项，气泡提醒一次
                if let Some(todo) = &mut self.todo {
                    if let Some(text) = todo.poll_due() {
                        if !self.hidden && !self.settings.quiet {
                            self.bubble_show(&format!("叮咚！待办到期：{text}"));
                        }
                    }
                }
            }
            Deadline::Onboard => {
                if let Some(diag) = self.startup_diag.take() {
                    self.bubble_show(&diag);
                } else if !self.settings.onboarded {
                    self.settings.onboarded = true;
                    self.persist_toggles();
                    self.bubble_show("右键我打开菜单喵！长按拖动、双击睡觉，拖张图片给我看看～");
                }
            }
            Deadline::Think => {
                if !self.chat_pending {
                    self.timers.cancel(Deadline::Think);
                    return;
                }
                self.think_frame = (self.think_frame + 1) % 3;
                let dots = format!(
                    "{}{}",
                    "·".repeat(self.think_frame as usize + 1),
                    " ".repeat(2 - self.think_frame as usize)
                );
                let (pp, ps, mon, above) = (self.pos, self.pet_size, self.mon, self.bubble_above());
                if let Some(b) = &mut self.bubble {
                    b.show_now(&dots, pp, ps, mon, above);
                }
            }
        }
    }

    fn about_to_wait_inner(&mut self, el: &ActiveEventLoop) {
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
        // 统一定时器：到期任务逐个 fire（fire 内可能设置新任务）
        while let Some(kind) = self.timers.pop_due(now) {
            self.fire_deadline(el, kind);
        }
        // 打字机气泡逐字推进
        if let Some(b) = &mut self.bubble {
            if let Some(t) = b.next_tick_at() {
                if now >= t {
                    b.advance_typing();
                }
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
        // 冒烟模式：全部窗口渲染到位 → 通过退出；超时 → 失败退出
        if self.selftest {
            #[cfg(windows)]
            let required: &[&str] = &["pet", "menu", "input", "todo"];
            #[cfg(not(windows))]
            let required: &[&str] = &["pet", "input", "todo"];
            if required.iter().all(|r| self.selftest_seen.contains(r)) {
                dlog("SELFTEST PASS：全部窗口渲染探活通过");
                std::process::exit(0);
            }
            if let Some(dl) = self.selftest_deadline {
                if Instant::now() >= dl {
                    let missing: Vec<&str> = required
                        .iter()
                        .filter(|r| !self.selftest_seen.contains(r))
                        .copied()
                        .collect();
                    dlog(&format!("SELFTEST FAILED：未收到渲染的窗口 {missing:?}"));
                    std::process::exit(1);
                }
            }
        }
        self.schedule(el);
    }

    // ---------- 派生窗口统一操作（新增窗口只需在这三个方法里登记） ----------

    /// 全屏遮挡/快捷键隐藏：收起全部派生窗口
    fn hide_derived(&mut self) {
        self.menu = None;
        self.bubble_hide();
        if let Some(i) = &self.input {
            i.window.set_visible(false);
        }
        if let Some(t) = &self.todo {
            t.window.set_visible(false);
        }
    }

    /// 从隐藏恢复：重开派生窗口
    fn restore_derived(&mut self) {
        if let Some(i) = &self.input {
            i.window.set_visible(true);
        }
        if let Some(t) = &self.todo {
            t.window.set_visible(true);
        }
    }

    /// UI 缩放热切换：气泡按新字号重建，输入框/待办/菜单关闭待重开
    fn rebuild_derived_for_scale(&mut self, el: &ActiveEventLoop) {
        self.menu = None;
        self.bubble_hide();
        self.bubble = BubbleWin::create(el);
        if let Some(i) = &self.input {
            i.window.set_visible(false);
        }
        self.input = None;
        if let Some(t) = &self.todo {
            t.window.set_visible(false);
        }
        self.todo = None;
    }

    fn set_hidden(&mut self, el: &ActiveEventLoop, hide: bool) {
        self.hidden = hide;
        // Arc clone（非借用）：hide/restore_derived 需要 &mut self
        let Some(window) = self.window.clone() else { return };
        window.set_visible(!hide);
        if hide {
            // 瞬态状态立即落地，避免隐藏期间动画停摆、恢复后冻结
            self.press = None;
            if matches!(
                self.state,
                PetState::Thrown | PetState::Climb | PetState::Dragged | PetState::Perch
            ) {
                self.perch = None;
                self.pos.1 = self.mon_bottom();
                window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
                self.timers.cancel(Deadline::Hang);
                self.climb_wall = 0;
                self.enter_idle();
            }
            self.hide_derived();
            self.frame_at = None;
            self.typing_until = None;
            el.set_control_flow(ControlFlow::Wait);
        } else {
            self.restore_derived();
            window.request_redraw();
        }
    }
}


impl ApplicationHandler<PetEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // UI 缩放：取主显示器 scale factor（100%/125%/150%/200%…）
        deskpet::set_ui_scale(el.primary_monitor().map(|m| m.scale_factor()).unwrap_or(1.0));
        dlog(&format!("UI 缩放：{:.2}", deskpet::ui_scale()));

        // 窗口尺寸必须按当前模型创建（512 高清猫 vs 64 经典物种）；
        // 曾用初值 64 创建、之后才更新 pet_size——512 画面 present 进
        // 64 窗口只剩透明左上角，整只猫"消失"
        self.pet_size = self.model.size().0 as i32;
        #[cfg_attr(not(windows), allow(unused_mut))] // windows 下会追加 with_skip_taskbar
        let mut attrs = Window::default_attributes()
            .with_inner_size(PhysicalSize::new(self.pet_size as u32, self.pet_size as u32))
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
        self.mons = collect_mons(el);
        if self.mons.is_empty() {
            self.mons = vec![MonRect { x: 0, y: 0, w: 1280, h: 720 }];
        }
        if let Some(cm) = window.current_monitor() {
            let (cx, cy) = (cm.position().x, cm.position().y);
            if let Some(m) = self.mons.iter().find(|m| m.x == cx && m.y == cy) {
                self.mon = *m;
            }
        }

        // 恢复上次位置（有记录且在当前显示器范围内）
        if let (Some(px), Some(py)) = (self.settings.pos_x, self.settings.pos_y) {
            let (px, py) = (px as i32, py as i32);
            if self.mon.contains_point(px, py) {
                self.pos = (px, py);
            }
        }
        if self.pos.1 + self.pet_size > self.mon.y + self.mon.h {
            self.pos = (
                (self.mon.x + self.mon.w - self.pet_size - 48).max(self.mon.x),
                (self.mon_bottom() - 96).max(self.mon.y),
            );
        }
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
        #[cfg(windows)]
        {
            let ok = deskpet::hwnd_of(&window).map(|h| unsafe { enable_colorkey(h) }).unwrap_or(false);
            dlog(if ok { "色键分层已启用（透明区域点击穿透）" } else { "色键分层启用失败" });
        }
        if surface
            .resize(
                NonZeroU32::new(self.pet_size as u32).unwrap(),
                NonZeroU32::new(self.pet_size as u32).unwrap(),
            )
            .is_err()
        {
            el.exit();
            return;
        }
        self.surface = Some(surface);
        self.window = Some(window.clone());
        self.pet_size = self.model.size().0 as i32;
        self.bubble = BubbleWin::create(el);
        self.refresh_model_registry();
        // 恢复上次模型：按名字匹配（增删模型后下标会漂移，名字不会）；
        // 旧配置只有下标——迁移一次并把名字写回配置
        let want = self.settings.model_name.clone().or_else(|| {
            self.settings
                .model_kind
                .and_then(|k| self.models.get(k).map(|(n, _)| n.clone()))
        });
        if let Some(name) = want {
            if let Some(i) = self.models.iter().position(|(n, _)| *n == name) {
                if i != 0 {
                    self.model_kind = i;
                    self.switch_model(i);
                }
            }
        }
        dlog(&format!(
            "显示器 {} 块，当前 {}x{}+{}+{}；气泡窗 {}",
            self.mons.len(),
            self.mon.w, self.mon.h, self.mon.x, self.mon.y,
            if self.bubble.is_some() { "OK" } else { "创建失败" }
        ));

        self.timers.set(Deadline::Whisper, Instant::now() + Duration::from_secs(25));
        self.arm_reminders();
        // 启动延迟消息：配置问题提示优先，否则首次运行引导（引导只出现一次）
        if !self.settings.onboarded || self.startup_diag.is_some() {
            let delay = if self.startup_diag.is_some() { 800 } else { 2500 };
            self.timers.set(Deadline::Onboard, Instant::now() + Duration::from_millis(delay));
        }
        // 周期任务：会话保存 / 待办截止轮询
        self.timers
            .set_recur(Deadline::Autosave, Instant::now() + Duration::from_secs(300), Duration::from_secs(300));
        self.timers
            .set_recur(Deadline::DueCheck, Instant::now() + Duration::from_secs(20), Duration::from_secs(20));

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
            input::win::spawn_hotkeys(self.proxy.clone());
            dlog("全局快捷键已注册（Ctrl+Shift+D/T/C/H/Q）");
        }

        window.request_redraw();
        dlog("启动完成");
        if self.selftest {
            // 冒烟：创建全部派生窗口并触发渲染，全部收到 Redraw 后 exit(0)
            self.ensure_input(el);
            self.ensure_todo(el);
            #[cfg(windows)]
            if self.menu.is_none() {
                let pages = vec![(Page::Root, self.root_entries())];
                if let Some(m) = MenuWin::create(el, pages, Page::Root) {
                    self.menu = Some(m.open((self.mon.x + 40, self.mon.y + 40), self.mon));
                }
            }
            self.bubble_show("SELFTEST 气泡渲染探活");
            self.selftest_deadline = Some(Instant::now() + Duration::from_secs(5));
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            dlog("SELFTEST：窗口渲染探活开始（5 秒超时）");
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.window_event_inner(el, window_id, event);
        }));
        if result.is_err() {
            self.recover_from_panic("window_event");
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: PetEvent) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.user_event_inner(el, event);
        }));
        if result.is_err() {
            self.recover_from_panic("user_event");
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.about_to_wait_inner(el);
        }));
        if result.is_err() {
            self.recover_from_panic("about_to_wait");
        }
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
        let w = (rect.right - rect.left).max(self.pet_size + 8);
        let off = 4 + (self.rand() % ((w - self.pet_size - 8).max(1) as u64)) as i32;
        self.perch = Some((addr, off, 40)); // ~7 秒
        self.enter_pose(PetState::Perch, None, Some("爬上来啦～")); // 时长由 perch ticks 决定
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
            self.pos.1 = (rect.top - self.pet_size + 3).max(self.mon.y);
        }
    }
}

/// 聊天记录落盘：chat_log.jsonl（exe 旁），每行一条 {ts, role, text}。
/// 超过 500 行时裁到 400；settings.chat_log 关闭时不写。
impl App {
    fn log_chat(&self, role: &str, text: &str) {
        if !self.settings.chat_log || text.is_empty() {
            return;
        }
        let Some(path) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("chat_log.jsonl")))
        else {
            return;
        };
        use std::io::Write;
        let line = serde_json::json!({
            "ts": deskpet::now_ms() / 1000,
            "role": role,
            "text": text,
        })
        .to_string();
        let open_result = std::fs::OpenOptions::new().create(true).append(true).open(&path);
        if let Ok(mut f) = open_result {
            if writeln!(f, "{line}").is_err() {
                dwarn("chat-log", "聊天记录写入失败");
            }
        } else {
            dwarn("chat-log", "聊天记录文件无法打开");
        }
        // 低频裁剪：追加后超过 500 行则重写保留最后 400 行
        if let Ok(content) = std::fs::read_to_string(&path) {
            let lines: Vec<&str> = content.lines().collect();
            if lines.len() > 500 {
                let kept = lines[lines.len() - 400..].join("\n");
                let _ = std::fs::write(&path, kept + "\n");
            }
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

/// 当前进程工作集（MB），关于气泡展示；读取失败返回 None
#[cfg(windows)]
fn working_set_mb() -> Option<f32> {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb).is_ok() {
            Some(counters.WorkingSetSize as f32 / 1024.0 / 1024.0)
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
fn working_set_mb() -> Option<f32> {
    None
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
        let mut mon_fp = monitor_fingerprint();
        loop {
            let fs = unsafe { foreground_is_fullscreen() };
            if fs != was {
                was = fs;
                let _ = proxy.send_event(PetEvent::FullscreenChanged(fs));
            }
            // 显示器热插拔/分辨率变化：指纹比对（2 秒一次，开销可忽略）
            let fp = monitor_fingerprint();
            if fp != mon_fp {
                mon_fp = fp;
                let _ = proxy.send_event(PetEvent::MonitorsChanged);
            }
            std::thread::sleep(Duration::from_millis(2000));
        }
    });
}

/// 所有显示器的拓扑指纹（位置+尺寸；数量变化或分辨率变化都会改变指纹）
#[cfg(windows)]
fn monitor_fingerprint() -> u64 {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    };
    unsafe extern "system" fn cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> windows::core::BOOL {
        let list = &mut *(lparam.0 as *mut Vec<(i32, i32, i32, i32)>);
        let mut info = MONITORINFO::default();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(hmon, &mut info).as_bool() {
            let r = info.rcMonitor;
            list.push((r.left, r.top, r.right - r.left, r.bottom - r.top));
        }
        true.into()
    }
    let mut list: Vec<(i32, i32, i32, i32)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(cb),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    let mut hash: u64 = 0xcbf29ce484222325;
    for &(x, y, w, h) in &list {
        for v in [x, y, w, h] {
            hash ^= v as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

#[cfg(not(windows))]
#[allow(dead_code)] // 非 windows 下 watcher 为空实现，占位保持签名一致
fn monitor_fingerprint() -> u64 {
    0
}

/// 枚举当前所有显示器（winit 视角，物理坐标）
fn collect_mons(el: &ActiveEventLoop) -> Vec<MonRect> {
    el.available_monitors()
        .into_iter()
        .map(|m| MonRect {
            x: m.position().x,
            y: m.position().y,
            w: m.size().width as i32,
            h: m.size().height as i32,
        })
        .filter(|m| m.w > 0 && m.h > 0)
        .collect()
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

/// 启动诊断日志已移至 lib（deskpet::dlog / deskpet::dwarn），
/// 库模块（config/todo）与主程序共用同一份 deskpet.log。

/// 单实例互斥：已有桌宠运行时直接退出（防止双开出现两只猫/资源冲突）
#[cfg(windows)]
fn ensure_single_instance() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        // Local 命名空间：会话内单实例即可，且无需 Global 所需的额外权限
        let name: Vec<u16> = "Local\\deskpet-rs-single-instance\0"
            .encode_utf16()
            .collect();
        let _ = CreateMutexW(None, false, PCWSTR(name.as_ptr()));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            dlog("已有桌宠实例在运行，本次启动退出");
            std::process::exit(0);
        }
    }
}

#[cfg(not(windows))]
fn ensure_single_instance() {}

#[cfg(windows)]
use windows::Win32::Foundation::GetLastError;

/// 色键：渲染中 alpha=0 的像素统一填成这个颜色，
/// 配合 WS_EX_LAYERED + LWA_COLORKEY 实现"视觉透明 + 点击穿透"。
/// 注意：美术素材里不要出现 RGB(254,1,254)。
pub const COLORKEY: u32 = 0xFFFE01FE;


#[cfg(windows)]
unsafe fn enable_colorkey(hwnd: isize) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE,
        LWA_COLORKEY, WS_EX_LAYERED,
    };
    use windows::Win32::Foundation::COLORREF;
    let h = HWND(hwnd as *mut core::ffi::c_void);
    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
    SetWindowLongPtrW(h, GWL_EXSTYLE, ex | (WS_EX_LAYERED.0 as isize));
    SetLayeredWindowAttributes(h, COLORREF(COLORKEY), 0, LWA_COLORKEY).is_ok()
}

/// --selftest 纯逻辑自检：全部内置模型 × 全部状态渲染非空、
/// models/ 下的帧序列模型加载渲染、字体行高、配置解析。返回失败清单。
fn run_selftest_logic() -> Vec<String> {
    let mut failures = Vec::new();
    let states = [
        PetState::Idle, PetState::Walk, PetState::Sleep, PetState::Patted,
        PetState::Shocked, PetState::Dragged, PetState::Thrown, PetState::Climb,
        PetState::Sitting, PetState::Stretch, PetState::Groom, PetState::Eat,
        PetState::Perch,
    ];
    for kind in 0..model_names().len() {
        let mut m = make_model(kind);
        for st in states {
            let pose = Pose {
                state: st, tick: 1, gaze: (1, 1), expr: 0, costume: 0, aux: -1, typing: false,
            };
            let out = m.render(&pose);
            if out.is_empty() || out.iter().all(|&p| p == 0) {
                failures.push(format!("内置模型{kind} 状态{st:?} 渲染为空"));
            }
        }
    }
    let found = deskpet::sprite_model::discover(std::path::Path::new("models"));
    if found.is_empty() {
        failures.push("models/ 下没有发现任何帧序列模型".into());
    }
    for (name, dir) in found {
        match SpriteModel::load(dir) {
            Some(mut m) => {
                let pose = Pose {
                    state: PetState::Idle, tick: 0, gaze: (0, 0), expr: 0,
                    costume: 0, aux: 0, typing: false,
                };
                if m.render(&pose).is_empty() {
                    failures.push(format!("帧序列模型 {name} 渲染为空"));
                }
            }
            None => failures.push(format!("帧序列模型 {name} 加载失败")),
        }
    }
    let lh = deskpet::text::TEXT.line_height(12);
    if !(12..=24).contains(&lh) {
        failures.push(format!("字体行高异常: {lh}"));
    }
    if deskpet::config::parse_config("").pet_name.is_empty() {
        failures.push("默认配置解析异常".into());
    }
    failures
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        dlog(&format!("PANIC: {info}"));
    }));
    dlog("启动：v0.3.2");
    ensure_single_instance();
    // --selftest：纯逻辑自检不通过直接退出；通过则继续窗口渲染探活
    let selftest = std::env::args().any(|a| a == "--selftest");
    if selftest {
        dlog("SELFTEST：逻辑自检开始");
        let failures = run_selftest_logic();
        if !failures.is_empty() {
            for f in &failures {
                dlog(&format!("SELFTEST 失败：{f}"));
            }
            dlog("SELFTEST FAILED");
            std::process::exit(1);
        }
        dlog("SELFTEST：逻辑自检通过，进入窗口渲染探活");
    }
    let text = deskpet::config::load_text();
    if text.is_none() && deskpet::config::ensure_default_template() {
        dlog("首次运行：已生成配置模板 deskpet.toml（全注释 = 离线默认）");
    }
    let cfg = text.as_deref().map(deskpet::config::parse_config).unwrap_or_default();
    let settings = deskpet::config::parse_settings(text.as_deref().unwrap_or(""));
    let startup_diag = deskpet::config::config_diag(text.as_deref());
    if let Some(d) = &startup_diag {
        dlog(d);
    }
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
    let mut app = App::new(cfg, settings, proxy, startup_diag);
    app.selftest = selftest;
    event_loop.run_app(&mut app).expect("事件循环异常退出");
}

/// 拖动锚定全局（纯函数，回归守护半速跟随 bug）：
/// CursorMoved 的本地坐标 + 事件时窗口位置（pos，尚未更新）= 鼠标全局坐标（恒等式）；
/// 减去按下时的抓取偏移（= 按下时本地坐标，因按下时窗口未动）即新窗口位置。
fn apply_drag(pos: (i32, i32), grab_offset: (f64, f64), cur_local: (f64, f64)) -> (i32, i32) {
    let gm = (cur_local.0 + pos.0 as f64, cur_local.1 + pos.1 as f64);
    ((gm.0 - grab_offset.0) as i32, (gm.1 - grab_offset.1) as i32)
}

/// 双击判定：距上次短点击松开 ≤400ms 且位移 <8px（纯函数）
fn is_double_click(
    now: Instant,
    last: (Instant, (f64, f64)),
    cur: (f64, f64),
) -> bool {
    now.duration_since(last.0) < Duration::from_millis(400)
        && (cur.0 - last.1 .0).abs() < 8.0
        && (cur.1 - last.1 .1).abs() < 8.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：拖动必须 1:1 跟手。模拟真实事件流——每个事件的本地坐标
    /// 基于上一步移动后的窗口位置。旧实现（本地坐标直接减抓取偏移）
    /// 稳态下窗口只有鼠标一半速度，本测试锁死正确行为。
    #[test]
    fn drag_follows_cursor_one_to_one() {
        // 起点：窗口 (100,100)，鼠标全局 130 → 按下时本地 (30,30) 即抓取偏移
        let grab = (30.0f64, 30.0f64);
        let mut pos = (100i32, 100i32);
        // 鼠标全局每步 +10；本地坐标 = 全局 - 当前窗口位置
        let mouse_global = [140i32, 150, 160, 170];
        for gx in mouse_global {
            let local = ((gx - pos.0) as f64, (gx - pos.0) as f64);
            pos = apply_drag(pos, grab, local);
        }
        // 鼠标全程 +40，窗口必须到 100+40（旧实现只能走到一半）
        assert_eq!(pos, (140, 140), "窗口位移应等于鼠标全程位移");
    }

    #[test]
    fn drag_handles_negative_local_coords() {
        // 捕获期间光标移出窗口左侧：本地坐标为负同样正确
        // 窗口 (100,100)，抓取偏移 30 → 鼠标全局 80，本地 = 80-100 = -20
        let pos = apply_drag((100, 100), (30.0, 30.0), (-20.0, -20.0));
        assert_eq!(pos, (50, 50), "鼠标在 80，窗口应保持 30 的抓取偏移");
    }

    #[test]
    fn double_click_requires_quick_release_then_press() {
        // 用真实时钟的小间隔：松开后立刻按下 = 双击
        let t = Instant::now();
        assert!(is_double_click(t + Duration::from_millis(100), (t, (50.0, 50.0)), (52.0, 51.0)));
        // 按下后隔 600ms 才再按：不是双击
        assert!(!is_double_click(t + Duration::from_millis(600), (t, (50.0, 50.0)), (50.0, 50.0)));
        // 时间够近但位移大（拖动释放后的按下）：不是双击
        assert!(!is_double_click(t + Duration::from_millis(100), (t, (50.0, 50.0)), (80.0, 80.0)));
    }
}
