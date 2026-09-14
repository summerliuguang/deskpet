//! 程序化绘制的像素猫（内置桌宠模型），零依赖、跨平台。
//!
//! 逻辑分辨率 16x16，放大 SCALE 倍输出。像素格式 0xAARRGGBB，
//! alpha=0 表示完全透明（二值透明，避免半透明混合的合成开销）。
//!
//! v0.3.2 美术升级：自动描边（剪影轮廓）、口鼻/胡须/内耳细节、
//! 新动作（端坐/伸懒腰/舔毛/吃饭/趴窗）、新表情（星星眼/脸红）。
//! 支持配色换装（Palette）和表情钉选（expr 索引，0=自动）。
//! 支持配色换装（Palette）和表情钉选（expr 索引，0=自动）。

pub const SPRITE_W: usize = 64;

const LOGICAL: i32 = 16;
const SCALE: i32 = (SPRITE_W as i32) / LOGICAL;

const DARK: u32 = 0xFF3A3026;
const PINK: u32 = 0xFFF08A8A;
const GOLD: u32 = 0xFFFFD24A;

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub name: &'static str,
    pub body: u32,
    pub belly: u32,
    pub outline: u32,
    pub eye: u32,
}

pub const COSTUMES: &[Palette] = &[
    Palette { name: "橘猫", body: 0xFFE8A33D, belly: 0xFFFFF3DC, outline: 0xFF6B4A1E, eye: DARK },
    Palette { name: "白猫", body: 0xFFEFEDE6, belly: 0xFFFBFAF6, outline: 0xFFB9B4A8, eye: 0xFF5A5348 },
    Palette { name: "黑猫", body: 0xFF474252, belly: 0xFF625C6E, outline: 0xFF232028, eye: 0xFFE8E4DA },
    Palette { name: "粉猫", body: 0xFFF3BCC9, belly: 0xFFFCE9EE, outline: 0xFFC98A9C, eye: 0xFF5A4A50 },
];

pub const EXPRESSIONS: &[&str] = &["自动", "开心", "惊讶", "困困", "星星眼", "脸红"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    IdleOpen,
    IdleBlink,
    Happy,
    Shock,
    WalkA,
    WalkB,
    DraggedA,
    DraggedB,
    Sleep,
    SitA,
    SitB,
    Stretch,
    GroomA,
    GroomB,
    EatA,
    EatB,
}

struct Canvas {
    px: Vec<u32>,
}

impl Canvas {
    /// 画一个逻辑像素（自动放大）
    fn dot(&mut self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 || x >= LOGICAL || y >= LOGICAL {
            return;
        }
        let (bx, by) = (x * SCALE, y * SCALE);
        for dy in 0..SCALE {
            for dx in 0..SCALE {
                let idx = ((by + dy) as usize) * SPRITE_W + (bx + dx) as usize;
                self.px[idx] = color;
            }
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.dot(x, y, color);
            }
        }
    }

    /// 描边通道：所有与不透明像素 4 邻接的透明像素染成描边色
    fn outline_pass(&mut self, color: u32) {
        let mut to_paint = Vec::new();
        for y in 0..LOGICAL {
            for x in 0..LOGICAL {
                if self.alpha_at(x, y) {
                    continue;
                }
                let neigh = self.alpha_at(x - 1, y)
                    || self.alpha_at(x + 1, y)
                    || self.alpha_at(x, y - 1)
                    || self.alpha_at(x, y + 1);
                if neigh {
                    to_paint.push((x, y));
                }
            }
        }
        for (x, y) in to_paint {
            self.dot(x, y, color);
        }
    }

    fn alpha_at(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= LOGICAL || y >= LOGICAL {
            return false;
        }
        let idx = ((y * SCALE) as usize) * SPRITE_W + (x * SCALE) as usize;
        self.px[idx] != 0
    }
}

/// 站姿猫主体（头 + 身体 + 口鼻/胡须/内耳细节）
fn base_cat(c: &mut Canvas, p: &Palette) {
    let (body, belly, pink) = (p.body, p.belly, PINK);
    // 耳朵
    c.rect(3, 1, 4, 2, body);
    c.rect(11, 1, 12, 2, body);
    // 头
    c.rect(3, 3, 12, 9, body);
    // 内耳
    c.rect(3, 2, 4, 2, pink);
    c.rect(11, 2, 12, 2, pink);
    // 身体
    c.rect(4, 10, 11, 14, body);
    // 肚子
    c.rect(6, 11, 9, 13, belly);
    // 前爪
    c.rect(4, 15, 5, 15, body);
    c.rect(10, 15, 11, 15, body);
    // 白口鼻
    c.rect(6, 7, 9, 8, belly);
    // 鼻子
    c.rect(7, 6, 8, 7, pink);
    // 胡须
    c.dot(1, 6, p.outline);
    c.dot(1, 8, p.outline);
    c.dot(14, 6, p.outline);
    c.dot(14, 8, p.outline);
}

/// 睁眼，瞳孔按 gaze=(gx,gy) ∈ {-1,0,1} 偏移（目光跟随）
fn open_eyes(c: &mut Canvas, gaze: (i32, i32), eye: u32) {
    let (gx, gy) = (gaze.0.clamp(-1, 1), gaze.1.clamp(-1, 1));
    c.rect(5 + gx, 4 + gy, 6 + gx, 5 + gy, eye);
    c.rect(9 + gx, 4 + gy, 10 + gx, 5 + gy, eye);
}

fn closed_eyes(c: &mut Canvas, eye: u32) {
    c.rect(5, 5, 6, 5, eye);
    c.rect(9, 5, 10, 5, eye);
}

/// 开心眯眼 ^^
fn happy_eyes(c: &mut Canvas, eye: u32) {
    c.dot(5, 5, eye);
    c.dot(6, 4, eye);
    c.dot(10, 5, eye);
    c.dot(9, 4, eye);
}

/// 惊吓小圆眼
fn shock_eyes(c: &mut Canvas, eye: u32) {
    c.dot(5, 4, eye);
    c.dot(10, 4, eye);
}

/// 星星眼：金色十字星
fn star_eyes(c: &mut Canvas) {
    for (cx, cy) in [(5, 4), (10, 4)] {
        c.dot(cx, cy, GOLD);
        c.dot(cx - 1, cy + 1, GOLD);
        c.dot(cx + 1, cy + 1, GOLD);
        c.dot(cx, cy + 2, GOLD);
        c.dot(cx, cy - 1, GOLD);
    }
}

/// 脸红腮红
fn blush(c: &mut Canvas) {
    c.rect(3, 6, 4, 6, PINK);
    c.rect(11, 6, 12, 6, PINK);
}

fn smile(c: &mut Canvas, outline: u32) {
    c.rect(6, 8, 9, 8, outline);
    c.dot(7, 9, outline);
    c.dot(8, 9, outline);
}

fn open_mouth(c: &mut Canvas, outline: u32) {
    c.rect(7, 8, 8, 9, outline);
}

fn neutral_mouth(c: &mut Canvas, outline: u32) {
    c.dot(6, 8, outline);
    c.dot(9, 8, outline);
}

fn tail_sway(c: &mut Canvas, up: bool, body: u32) {
    if up {
        c.rect(13, 8, 13, 13, body);
        c.dot(12, 7, body);
        c.dot(13, 6, PINK); // 尾尖
    } else {
        c.rect(13, 12, 13, 14, body);
        c.rect(12, 11, 12, 13, body);
        c.dot(13, 14, PINK);
    }
}

fn tail_up(c: &mut Canvas, body: u32) {
    c.rect(13, 5, 13, 13, body);
    c.dot(12, 4, body);
    c.dot(13, 4, PINK);
}

fn draw_zzz(c: &mut Canvas) {
    // 两个小 Z，表示睡着
    for (zx, zy) in [(12, 1), (14, 4)] {
        c.rect(zx, zy, zx + 1, zy, BLUE_OK);
        c.dot(zx + 1, zy + 1, BLUE_OK);
        c.rect(zx, zy + 2, zx + 1, zy + 2, BLUE_OK);
    }
}

const BLUE_OK: u32 = 0xFF5A8CFF;

/// 表情钉选（idx>0 时覆盖 Idle/Walk/Sit 脸部）
fn pinned_face(c: &mut Canvas, p: &Palette, expr: usize) {
    let eye = p.eye;
    match expr {
        1 => {
            happy_eyes(c, eye);
            smile(c, p.outline);
        }
        2 => {
            shock_eyes(c, eye);
            open_mouth(c, p.outline);
        }
        3 => closed_eyes(c, eye),
        4 => star_eyes(c),
        5 => {
            open_eyes(c, (0, 0), eye);
            blush(c);
            smile(c, p.outline);
        }
        _ => {}
    }
}

/// Pose → 帧
pub fn pose_to_frame(pose: &crate::model::Pose) -> Frame {
    use crate::model::PetState::*;
    let step = pose.tick % 2 == 0;
    match pose.state {
        Sleep => Frame::Sleep,
        Patted => Frame::Happy,
        Shocked | Thrown => Frame::Shock,
        Dragged => {
            if step { Frame::DraggedA } else { Frame::DraggedB }
        }
        Walk | Climb => {
            if step { Frame::WalkA } else { Frame::WalkB }
        }
        Sitting | Perch => {
            if pose.tick % 8 < 6 { Frame::SitA } else { Frame::SitB }
        }
        Stretch => Frame::Stretch,
        Groom => {
            if step { Frame::GroomA } else { Frame::GroomB }
        }
        Eat => {
            if step { Frame::EatA } else { Frame::EatB }
        }
        Idle => {
            if pose.typing {
                if step { Frame::WalkA } else { Frame::WalkB }
            } else if pose.tick % 8 == 7 {
                Frame::IdleBlink
            } else {
                Frame::IdleOpen
            }
        }
    }
}

/// 渲染一帧。expr: 0=自动跟随状态，否则钉选表情（对 Idle/Walk/Sit/Climb 生效）。
pub fn render_frame(frame: Frame, gaze: (i32, i32), p: &Palette, expr: usize) -> Vec<u32> {
    let mut out = Vec::new();
    render_frame_into(&mut out, frame, gaze, p, expr);
    out
}

/// 复用调用方缓冲的零分配版本（供模型层每帧调用）
pub fn render_frame_into(
    out: &mut Vec<u32>,
    frame: Frame,
    gaze: (i32, i32),
    p: &Palette,
    expr: usize,
) {
    let (body, outline, eye) = (p.body, p.outline, p.eye);
    let mut c = Canvas { px: std::mem::take(out) };
    c.px.clear();
    c.px.resize(SPRITE_W * SPRITE_W, 0);
    match frame {
        Frame::IdleOpen => {
            base_cat(&mut c, p);
            if expr > 0 {
                pinned_face(&mut c, p, expr);
            } else {
                open_eyes(&mut c, gaze, eye);
                neutral_mouth(&mut c, outline);
            }
            tail_sway(&mut c, false, body);
        }
        Frame::IdleBlink => {
            base_cat(&mut c, p);
            closed_eyes(&mut c, eye);
            neutral_mouth(&mut c, outline);
            tail_sway(&mut c, false, body);
        }
        Frame::Happy => {
            base_cat(&mut c, p);
            happy_eyes(&mut c, eye);
            smile(&mut c, outline);
            tail_sway(&mut c, true, body);
        }
        Frame::Shock => {
            base_cat(&mut c, p);
            shock_eyes(&mut c, eye);
            open_mouth(&mut c, outline);
            tail_up(&mut c, body);
        }
        Frame::WalkA | Frame::WalkB => {
            base_cat(&mut c, p);
            if expr > 0 {
                pinned_face(&mut c, p, expr);
            } else {
                open_eyes(&mut c, gaze, eye);
                neutral_mouth(&mut c, outline);
            }
            if frame == Frame::WalkA {
                tail_sway(&mut c, true, body);
                c.rect(4, 13, 5, 14, body); // 左前爪抬起
            } else {
                tail_sway(&mut c, false, body);
                c.rect(10, 13, 11, 14, body); // 右前爪抬起
            }
        }
        Frame::DraggedA | Frame::DraggedB => {
            // 被拎起来：惊吓脸 + 前爪朝上 + 后腿悬空乱蹬
            base_cat(&mut c, p);
            shock_eyes(&mut c, eye);
            open_mouth(&mut c, outline);
            tail_up(&mut c, body);
            c.rect(3, 9, 4, 10, body); // 前爪举起
            c.rect(11, 9, 12, 10, body);
            c.rect(5, 15, 6, 15, body);
            c.rect(9, 15, 10, 15, body);
            if frame == Frame::DraggedA {
                c.rect(4, 15, 4, 15, body);
                c.rect(11, 15, 11, 15, body);
            } else {
                c.rect(7, 15, 8, 15, body);
            }
        }
        Frame::SitA | Frame::SitB => {
            // 端坐：身体压实，尾巴绕到身前
            c.rect(4, 8, 11, 15, body);
            c.rect(3, 1, 4, 2, body);
            c.rect(11, 1, 12, 2, body);
            c.rect(3, 3, 12, 9, body);
            c.rect(7, 6, 8, 7, PINK);
            c.rect(6, 10, 9, 14, p.belly);
            if expr > 0 {
                pinned_face(&mut c, p, expr);
            } else {
                open_eyes(&mut c, gaze, eye);
                neutral_mouth(&mut c, outline);
            }
            c.rect(5, 15, 6, 15, body);
            c.rect(9, 15, 10, 15, body);
            if frame == Frame::SitB {
                // 尾巴尖翘起
                c.rect(3, 14, 3, 15, body);
                c.dot(2, 13, body);
            } else {
                c.rect(3, 15, 4, 15, body);
                c.rect(2, 15, 2, 15, PINK);
            }
        }
        Frame::Stretch => {
            // 伸懒腰：前低后高（侧视）
            c.rect(1, 9, 6, 13, body); // 前半身压低
            c.rect(6, 6, 13, 11, body); // 后半身抬高
            c.rect(0, 12, 1, 14, body); // 前爪前伸
            c.rect(12, 11, 14, 14, body); // 后腿
            c.rect(14, 3, 15, 7, body); // 尾巴朝天
            c.rect(2, 10, 4, 12, p.belly);
            closed_eyes(&mut c, eye);
            c.rect(2, 12, 3, 12, PINK); // 嘴
        }
        Frame::GroomA | Frame::GroomB => {
            // 舔毛：端坐 + 抬爪捂嘴
            c.rect(4, 8, 11, 15, body);
            c.rect(3, 1, 4, 2, body);
            c.rect(11, 1, 12, 2, body);
            c.rect(3, 3, 12, 9, body);
            closed_eyes(&mut c, eye);
            let paw_y = if frame == Frame::GroomA { 5 } else { 8 };
            c.rect(6, paw_y, 8, paw_y + 3, body);
            c.rect(6, paw_y, 8, paw_y, p.belly);
        }
        Frame::EatA | Frame::EatB => {
            // 低头吃饭：身后坐姿 + 头埋进碗
            c.rect(5, 4, 12, 11, body); // 抬高的后半身
            c.rect(3, 6, 5, 8, body); // 耳朵侧影
            c.rect(3, 7, 12, 13, body); // 低下的头
            closed_eyes(&mut c, eye);
            let dip = if frame == Frame::EatA { 0 } else { 1 };
            c.rect(4, 14, 12, 15, 0xFF8A7A6A); // 碗
            c.rect(5 + dip, 13, 11 + dip, 14, p.belly); // 碗里
        }
        Frame::Sleep => {
            // 蜷缩：压扁的身体 + 闭眼 + Zzz
            c.rect(2, 9, 13, 15, body);
            c.rect(3, 6, 9, 10, body);
            c.rect(3, 1, 4, 2, body);
            c.rect(8, 1, 9, 2, body);
            closed_eyes(&mut c, eye);
            c.rect(10, 12, 12, 13, p.belly);
            c.rect(13, 13, 15, 14, body);
            draw_zzz(&mut c);
        }
    }
    // 自动描边：所有剪影外一圈染描边色
    c.outline_pass(outline);
    *out = c.px;
}

/// 将 64x64 缓冲旋转 90 度：ccw=false 顺时针（左墙头朝上），true 逆时针（右墙）
pub fn rotate90_into(out: &mut Vec<u32>, src: &[u32], ccw: bool) {
    let n = SPRITE_W;
    out.clear();
    out.resize(n * n, 0);
    for y in 0..n {
        for x in 0..n {
            let (nx, ny) = if ccw { (y, n - 1 - x) } else { (n - 1 - y, x) };
            out[ny * n + nx] = src[y * n + x];
        }
    }
}

/// 托盘图标用 RGBA（32x32，由 64x64 帧隔行采样）
pub fn tray_icon_rgba() -> Vec<u8> {
    let full = render_frame(Frame::IdleOpen, (0, 0), &COSTUMES[0], 0);
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in (0..SPRITE_W).step_by(2) {
        for x in (0..SPRITE_W).step_by(2) {
            let p = full[y * SPRITE_W + x];
            let (a, r, g, b) = ((p >> 24) as u8, (p >> 16) as u8, (p >> 8) as u8, p as u8);
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    rgba
}

/// 把 ARGB 帧转成 RGB8（供调试导出 PPM 用，透明像素填白底）
pub fn argb_to_rgb(frame: &[u32]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(frame.len() * 3);
    for &p in frame {
        if p >> 24 == 0 {
            rgb.extend_from_slice(&[255, 255, 255]);
        } else {
            rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PetState, Pose};

    fn pose(state: PetState, tick: u32) -> Pose {
        Pose { state, tick, gaze: (0, 0), expr: 0, costume: 0, aux: 0, typing: false }
    }

    #[test]
    fn pose_to_frame_mapping() {
        assert_eq!(pose_to_frame(&pose(PetState::Sleep, 0)), Frame::Sleep);
        assert_eq!(pose_to_frame(&pose(PetState::Patted, 3)), Frame::Happy);
        assert_eq!(pose_to_frame(&pose(PetState::Shocked, 3)), Frame::Shock);
        assert_eq!(pose_to_frame(&pose(PetState::Thrown, 3)), Frame::Shock);
        assert_eq!(pose_to_frame(&pose(PetState::Walk, 0)), Frame::WalkA);
        assert_eq!(pose_to_frame(&pose(PetState::Walk, 1)), Frame::WalkB);
        assert_eq!(pose_to_frame(&pose(PetState::Idle, 7)), Frame::IdleBlink);
        assert_eq!(pose_to_frame(&pose(PetState::Idle, 0)), Frame::IdleOpen);
        assert_eq!(pose_to_frame(&pose(PetState::Sitting, 0)), Frame::SitA);
        assert_eq!(pose_to_frame(&pose(PetState::Sitting, 7)), Frame::SitB);
        assert_eq!(pose_to_frame(&pose(PetState::Stretch, 0)), Frame::Stretch);
        assert_eq!(pose_to_frame(&pose(PetState::Groom, 1)), Frame::GroomB);
        assert_eq!(pose_to_frame(&pose(PetState::Eat, 0)), Frame::EatA);
        assert_eq!(pose_to_frame(&pose(PetState::Perch, 3)), Frame::SitA);
    }

    #[test]
    fn all_frames_render_for_all_costumes() {
        for ci in 0..COSTUMES.len() {
            for f in [
                Frame::IdleOpen, Frame::IdleBlink, Frame::Happy, Frame::Shock,
                Frame::WalkA, Frame::WalkB, Frame::DraggedA, Frame::DraggedB, Frame::Sleep,
                Frame::SitA, Frame::SitB, Frame::Stretch, Frame::GroomA, Frame::GroomB,
                Frame::EatA, Frame::EatB,
            ] {
                let buf = render_frame(f, (1, -1), &COSTUMES[ci], 0);
                assert_eq!(buf.len(), SPRITE_W * SPRITE_W);
                assert!(buf.iter().any(|&p| p != 0), "帧不能全透明 {f:?} 皮肤{ci}");
                assert!(buf.iter().any(|&p| p == COSTUMES[ci].outline), "应有描边 {f:?}");
            }
        }
    }

    #[test]
    fn model_renders_climb_rotated() {
        use crate::model::{PixelCat, PetModel};
        let mut m = PixelCat::new();
        let base = pose(PetState::Walk, 0);
        // 渲染缓冲是复用的：需要留存的帧先 to_vec
        let upright = m.render(&base).to_vec();
        let climbed = m.render(&Pose { state: PetState::Climb, aux: -1, ..base });
        assert_ne!(upright.as_slice(), climbed, "爬墙帧应有旋转");
    }
}
