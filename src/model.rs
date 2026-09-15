//! 桌宠模型抽象层。
//!
//! 所有桌宠形象实现 [`PetModel`] trait：输入 Pose（状态/帧号/目光/表情/换装），
//! 输出一块 ARGB 像素切片（内部缓冲复用，每帧零分配）。
//!
//! 已定案：采用预设动作 + 交互表情方案（程序化像素猫 + 帧序列模型两种实现），
//! 不引入 Live2D 运行时。想要新形象 = 另写一个 PetModel 实现，或用帧序列模型
//! （models/ 目录放 PNG 帧），主程序状态机、菜单、交互零改动。

use crate::sprites;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PetState {
    Idle,
    Walk,
    Sleep,
    Patted,
    Shocked,
    Dragged,
    /// 被甩飞（拖拽释放带速度）
    Thrown,
    /// 爬屏幕边缘
    Climb,
    /// 端坐（休息姿势之一）
    Sitting,
    /// 伸懒腰
    Stretch,
    /// 舔毛/洗脸
    Groom,
    /// 吃东西（低头对着碗）
    Eat,
    /// 趴在别的窗口顶边上
    Perch,
}

/// 渲染输入。aux 的含义由状态决定：Climb 时为墙面方向（-1 左墙 / 1 右墙）。
#[derive(Debug, Clone, Copy)]
pub struct Pose {
    pub state: PetState,
    pub tick: u32,
    pub gaze: (i32, i32),
    /// 钉选表情索引（0=自动跟随状态）
    pub expr: usize,
    pub costume: usize,
    pub aux: i32,
    /// 键盘联动：像打字一样交替拍爪
    pub typing: bool,
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub costumes: Vec<String>,
    pub expressions: Vec<String>,
}

/// 桌宠模型接口。实现要求：Send（会被 App 持有），render 纯软件渲染。
pub trait PetModel: Send {
    fn info(&self) -> &ModelInfo;
    fn size(&self) -> (u32, u32) {
        (64, 64)
    }
    /// 渲染到内部复用缓冲并返回切片（每帧零分配）
    fn render(&mut self, pose: &Pose) -> &[u32];
    /// 该状态下的动画帧间隔（毫秒）；帧序列模型用自己的 fps
    fn frame_ms(&self, state: &PetState) -> u64 {
        let _ = state;
        600
    }
    fn set_costume(&mut self, idx: usize);
    fn set_expression(&mut self, idx: usize);
    fn costume(&self) -> usize;
    fn expression(&self) -> usize;
}

/// 内置像素宠物（猫/柴犬/兔兔/企鹅共用骨骼，不同外形）
pub struct PixelPet {
    species: sprites::Species,
    /// 精灵放大倍数（逻辑 16px × 倍数 = 窗口边长；随 UI 缩放取整数倍保证锐利）
    sprite_scale: i32,
    costume: usize,
    expr: usize,
    info: ModelInfo,
    /// 复用的渲染缓冲（避免每帧分配）
    buf: Vec<u32>,
    /// 爬墙旋转输出缓冲
    rot_buf: Vec<u32>,
}

impl PixelPet {
    pub fn new(kind: usize) -> Self {
        let species = sprites::species_of(kind);
        // 像素画只在整数倍缩放下锐利：逻辑 16px × 4 倍基准，随 UI 缩放取最近整数倍
        let sprite_scale = (4.0 * crate::ui_scale()).round().max(1.0) as i32;
        Self {
            species,
            sprite_scale,
            costume: 0,
            expr: 0,
            buf: Vec::with_capacity((16 * sprite_scale) as usize * (16 * sprite_scale) as usize),
            rot_buf: Vec::with_capacity((16 * sprite_scale) as usize * (16 * sprite_scale) as usize),
            info: ModelInfo {
                name: sprites::SPECIES_NAMES[kind.min(sprites::SPECIES_NAMES.len() - 1)].into(),
                costumes: sprites::palettes(species).iter().map(|c| c.name.to_string()).collect(),
                expressions: sprites::EXPRESSIONS.iter().map(|s| s.to_string()).collect(),
            },
        }
    }
}

/// 内置模型清单（模型选择页用）
pub fn model_names() -> Vec<String> {
    sprites::SPECIES_NAMES.iter().map(|s| s.to_string()).collect()
}

/// 按种类构建模型
pub fn make_model(kind: usize) -> Box<dyn PetModel> {
    Box::new(PixelPet::new(kind))
}

impl PetModel for PixelPet {
    fn info(&self) -> &ModelInfo {
        &self.info
    }

    fn size(&self) -> (u32, u32) {
        let side = (16 * self.sprite_scale) as u32;
        (side, side)
    }

    fn render(&mut self, pose: &Pose) -> &[u32] {
        let pals = sprites::palettes(self.species);
        let palette = &pals[self.costume.min(pals.len() - 1)];
        let frame = sprites::pose_to_frame(pose);
        sprites::render_frame_into(
            &mut self.buf,
            frame,
            pose.gaze,
            palette,
            pose.expr,
            self.species,
            self.sprite_scale,
        );
        if pose.state == PetState::Climb && pose.aux != 0 {
            sprites::rotate90_into(&mut self.rot_buf, &self.buf, pose.aux < 0, (16 * self.sprite_scale) as usize);
            return &self.rot_buf;
        }
        &self.buf
    }

    fn frame_ms(&self, state: &PetState) -> u64 {
        match state {
            PetState::Walk | PetState::Climb | PetState::Perch => 180,
            PetState::Dragged => 140,
            PetState::Thrown => 16,
            PetState::Groom | PetState::Eat => 200,
            PetState::Stretch => 400,
            _ => 600,
        }
    }

    fn set_costume(&mut self, idx: usize) {
        self.costume = idx;
    }
    fn set_expression(&mut self, idx: usize) {
        self.expr = idx;
    }
    fn costume(&self) -> usize {
        self.costume
    }
    fn expression(&self) -> usize {
        self.expr
    }
}
