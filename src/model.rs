//! 桌宠模型抽象层。
//!
//! 所有桌宠形象实现 [`PetModel`] trait：输入 Pose（状态/帧号/目光/表情/换装），
//! 输出一块 ARGB 像素。内置实现是程序化像素猫；将来 Live2D/自制素材模型
//! 只需另写一个 PetModel 实现，主程序和菜单无需改动。

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
    fn render(&mut self, pose: &Pose) -> Vec<u32>;
    fn set_costume(&mut self, idx: usize);
    fn set_expression(&mut self, idx: usize);
    fn costume(&self) -> usize;
    fn expression(&self) -> usize;
}

/// 内置像素猫
pub struct PixelCat {
    costume: usize,
    expr: usize,
    info: ModelInfo,
}

impl PixelCat {
    pub fn new() -> Self {
        Self {
            costume: 0,
            expr: 0,
            info: ModelInfo {
                name: "像素猫".into(),
                costumes: sprites::COSTUMES.iter().map(|c| c.name.to_string()).collect(),
                expressions: sprites::EXPRESSIONS.iter().map(|s| s.to_string()).collect(),
            },
        }
    }
}

impl Default for PixelCat {
    fn default() -> Self {
        Self::new()
    }
}

impl PetModel for PixelCat {
    fn info(&self) -> &ModelInfo {
        &self.info
    }

    fn render(&mut self, pose: &Pose) -> Vec<u32> {
        let palette = &sprites::COSTUMES[self.costume.min(sprites::COSTUMES.len() - 1)];
        let frame = sprites::pose_to_frame(pose);
        let mut buf = sprites::render_frame(frame, pose.gaze, palette, pose.expr);
        if pose.state == PetState::Climb && pose.aux != 0 {
            buf = rotate90(buf, pose.aux < 0);
        }
        buf
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

/// 将 64x64 缓冲旋转 90 度：ccw=false 顺时针（左墙头朝上），true 逆时针（右墙）
pub fn rotate90(buf: Vec<u32>, ccw: bool) -> Vec<u32> {
    let n = crate::sprites::SPRITE_W;
    let mut out = vec![0u32; n * n];
    for y in 0..n {
        for x in 0..n {
            let v = buf[y * n + x];
            let (nx, ny) = if ccw { (y, n - 1 - x) } else { (n - 1 - y, x) };
            out[ny * n + nx] = v;
        }
    }
    out
}
