//! 高清像素猫管线：32×32 逻辑格 × 16 倍 = 512×512。
//!
//! 几何与 `/tmp` 原型生成器（参数化椭圆/三角/描边）1:1 对应；
//! 颜色走调色板索引（换装 = 换调色板），仅 Species::Cat 使用本管线，
//! 其他物种继续走 sprites.rs 的 16×16 旧管线。

use crate::model::{PetState, Pose};
use crate::sprites::Palette;

pub const GRID: i32 = 32;
pub const SCALE: i32 = 16;
pub const SIDE: i32 = GRID * SCALE; // 512

pub const CAT_EXPRESSIONS: &[&str] = &[
    "自动", "开心", "惊讶", "闭眼", "星星眼", "爱心眼", "眨眼", "脸红", "吐舌",
];

/// 部件索引（渲染时查调色板）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Outline,
    Body,
    Belly,
    Pink,
    Eye,
    White,
    Blush,
    Gold,
}

impl Part {
    fn color(self, p: &Palette) -> u32 {
        match self {
            Part::Outline => p.outline,
            Part::Body => p.body,
            Part::Belly => p.belly,
            Part::Pink => 0xFFF08A8A,
            Part::Eye => p.eye,
            Part::White => 0xFFFFFFFF,
            Part::Blush => 0xFFF0A0A0,
            Part::Gold => 0xFFFFD24A,
        }
    }
}

const T: u32 = 0; // 透明标记（部件颜色全部 0xFF 开头，不会撞 0；白=0xFFFFFFFF 不再误判透明）

struct Canvas {
    px: Vec<u32>,
}

impl Canvas {
    fn new() -> Self {
        Self { px: vec![T; (GRID * GRID) as usize] }
    }

    fn at(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= GRID || y >= GRID { T } else { self.px[(y * GRID + x) as usize] }
    }

    fn set(&mut self, x: i32, y: i32, part: Part, p: &Palette) {
        if x < 0 || y < 0 || x >= GRID || y >= GRID { return; }
        self.px[(y * GRID + x) as usize] = part.color(p);
    }

    fn ellipse(&mut self, cx: i32, cy: i32, rx: i32, ry: i32, part: Part, pal: &Palette) {
        let rxf = rx.max(1) as f32;
        let ryf = ry.max(1) as f32;
        for y in 0..GRID {
            for x in 0..GRID {
                let dx = (x - cx) as f32 / rxf;
                let dy = (y - cy) as f32 / ryf;
                if dx * dx + dy * dy <= 1.0 {
                    self.set(x, y, part, pal);
                }
            }
        }
    }

    fn tri(&mut self, pts: [(i32, i32); 3], part: Part, pal: &Palette) {
        let (x0, y0) = pts[0];
        let (x1, y1) = pts[1];
        let (x2, y2) = pts[2];
        let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
        if area == 0 { return; }
        let minx = x0.min(x1).min(x2);
        let maxx = x0.max(x1).max(x2);
        let miny = y0.min(y1).min(y2);
        let maxy = y0.max(y1).max(y2);
        for y in miny..=maxy {
            for x in minx..=maxx {
                let w0 = (x1 - x0) * (y - y0) - (x - x0) * (y1 - y0);
                let w1 = (x2 - x1) * (y - y1) - (x - x1) * (y2 - y1);
                let w2 = (x0 - x2) * (y - y2) - (x - x2) * (y0 - y2);
                if (w0 >= 0 && w1 >= 0 && w2 >= 0) || (w0 <= 0 && w1 <= 0 && w2 <= 0) {
                    self.set(x, y, part, pal);
                }
            }
        }
    }

    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, part: Part, pal: &Palette) {
        for y in y0.max(0)..=y1.min(GRID - 1) {
            for x in x0.max(0)..=x1.min(GRID - 1) {
                self.set(x, y, part, pal);
            }
        }
    }

    fn outline_pass(&mut self, pal: &Palette) {
        let mut to_paint = Vec::new();
        for y in 0..GRID {
            for x in 0..GRID {
                if self.at(x, y) != T { continue; }
                for (ddx, ddy) in [(1i32, 0), (-1, 0), (0, 1), (0, -1)] {
                    if self.at(x + ddx, y + ddy) != T {
                        to_paint.push((x, y));
                        break;
                    }
                }
            }
        }
        for (x, y) in to_paint {
            self.set(x, y, Part::Outline, pal);
        }
    }

}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Eye { Open, Happy, Closed, Shock, Wink, Star, Heart }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tail { Right, Up, Wrap, Out }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paw { Down, Up }

/// 一次 draw_cat 的全部可变参数（与设计原型一一对应）
struct CatPose {
    eye: Eye,
    mouth_open: bool,
    tongue: bool,
    tail: Tail,
    tail_dx: i32,
    body_dx: i32,
    body_dy: i32,
    head_dy: i32,
    paw_left: Paw,
    paw_right: Paw,
    blush: bool,
    lying: bool,
    sparkle: bool,
    heart: bool,
    zzz: bool,
    ball: bool,
    belly: bool,
}

impl Default for CatPose {
    fn default() -> Self {
        Self {
            eye: Eye::Open,
            mouth_open: false,
            tongue: false,
            tail: Tail::Right,
            tail_dx: 0,
            body_dx: 0,
            body_dy: 0,
            head_dy: 0,
            paw_left: Paw::Down,
            paw_right: Paw::Down,
            blush: false,
            lying: false,
            sparkle: false,
            heart: false,
            zzz: false,
            ball: false,
            belly: true,
        }
    }
}

fn draw_cat(c: &mut Canvas, p: &CatPose, pal: &Palette) {
    let bdx = p.body_dx;
    let bdy = p.body_dy;

    // 尾巴（最底层）
    match p.tail {
        Tail::Right => {
            c.ellipse(27 + p.tail_dx, 24 + bdy, 2, 2, Part::Body, pal);
            c.ellipse(29 + p.tail_dx, 20 + bdy, 2, 2, Part::Body, pal);
            c.ellipse(29 + p.tail_dx, 16 + bdy, 2, 3, Part::Body, pal);
        }
        Tail::Up => {
            c.ellipse(27 + p.tail_dx, 22 + bdy, 2, 2, Part::Body, pal);
            c.ellipse(29 + p.tail_dx, 17 + bdy, 2, 3, Part::Body, pal);
        }
        Tail::Wrap => {
            c.ellipse(26 + p.tail_dx, 27 + bdy, 2, 2, Part::Body, pal);
            c.ellipse(22 + p.tail_dx, 29 + bdy, 3, 2, Part::Body, pal);
        }
        Tail::Out => {
            c.ellipse(4 + bdx, 24 + bdy, 3, 2, Part::Body, pal);
        }
    }

    if p.lying {
        // 躺平：横椭圆身体 + 头在左
        c.ellipse(16 + bdx, 22 + bdy, 12, 7, Part::Body, pal);
        c.ellipse(14 + bdx, 23 + bdy, 8, 4, Part::Belly, pal);
        c.ellipse(7 + bdx, 15 + bdy, 7, 6, Part::Body, pal);
        c.tri([(2 + bdx, 12 + bdy), (6 + bdx, 10 + bdy), (7 + bdx, 15 + bdy)], Part::Body, pal);
        c.tri([(12 + bdx, 10 + bdy), (14 + bdx, 14 + bdy), (9 + bdx, 12 + bdy)], Part::Body, pal);
    } else {
        c.ellipse(16 + bdx, 23 + bdy, 10, 7, Part::Body, pal);
        if p.belly {
            c.ellipse(16 + bdx, 24 + bdy, 6, 5, Part::Belly, pal);
        }
        c.ellipse(16 + bdx, 10 + p.head_dy, 11, 9, Part::Body, pal);
    }

    let hx = 16 + bdx;
    let hy = 10 + p.head_dy;
    // 耳朵
    c.tri([(hx - 9, hy - 4), (hx - 5, hy - 9), (hx - 3, hy - 3)], Part::Body, pal);
    c.tri([(hx + 9, hy - 4), (hx + 5, hy - 9), (hx + 3, hy - 3)], Part::Body, pal);
    c.tri([(hx - 7, hy - 4), (hx - 5, hy - 7), (hx - 4, hy - 4)], Part::Pink, pal);
    c.tri([(hx + 7, hy - 4), (hx + 5, hy - 7), (hx + 4, hy - 4)], Part::Pink, pal);

    // 眼睛
    let exl = hx - 5;
    let exr = hx + 5;
    let ey = hy - 1;
    match p.eye {
        Eye::Open => {
            c.ellipse(exl, ey, 2, 3, Part::Eye, pal);
            c.ellipse(exr, ey, 2, 3, Part::Eye, pal);
            c.rect(exl - 1, ey - 2, exl - 1, ey - 1, Part::White, pal);
            c.rect(exr - 1, ey - 2, exr - 1, ey - 1, Part::White, pal);
        }
        Eye::Happy => {
            c.rect(exl - 2, ey - 1, exl - 1, ey - 1, Part::Eye, pal);
            c.rect(exl, ey, exl, ey, Part::Eye, pal);
            c.rect(exr + 1, ey - 1, exr, ey - 1, Part::Eye, pal);
            c.rect(exr, ey, exr, ey, Part::Eye, pal);
        }
        Eye::Closed => {
            c.rect(exl - 2, ey, exl, ey, Part::Eye, pal);
            c.rect(exr - 1, ey, exr + 1, ey, Part::Eye, pal);
        }
        Eye::Shock => {
            c.ellipse(exl, ey, 3, 3, Part::Eye, pal);
            c.ellipse(exr, ey, 3, 3, Part::Eye, pal);
            c.rect(exl - 1, ey - 1, exl, ey, Part::White, pal);
            c.rect(exr - 1, ey - 1, exr, ey, Part::White, pal);
        }
        Eye::Wink => {
            c.ellipse(exl, ey, 2, 3, Part::Eye, pal);
            c.rect(exl - 1, ey - 2, exl - 1, ey - 1, Part::White, pal);
            c.rect(exr - 2, ey, exr + 1, ey, Part::Eye, pal);
        }
        Eye::Star => {
            for ex in [exl, exr] {
                c.rect(ex - 1, ey - 2, ex + 1, ey + 2, Part::Gold, pal);
                c.rect(ex - 2, ey - 1, ex + 2, ey + 1, Part::Gold, pal);
                c.rect(ex, ey, ex, ey, Part::Eye, pal);
            }
        }
        Eye::Heart => {
            for ex in [exl, exr] {
                c.rect(ex - 2, ey - 1, ex - 1, ey, Part::Pink, pal);
                c.rect(ex + 1, ey - 1, ex + 2, ey, Part::Pink, pal);
                c.rect(ex - 1, ey + 1, ex + 1, ey + 2, Part::Pink, pal);
            }
        }
    }

    // 鼻子和嘴
    c.rect(hx - 1, hy + 2, hx, hy + 2, Part::Pink, pal);
    if p.mouth_open {
        c.ellipse(hx, hy + 4, 2, 2, Part::Eye, pal);
        if p.tongue {
            c.ellipse(hx, hy + 5, 1, 1, Part::Pink, pal);
        }
    } else {
        c.rect(hx - 1, hy + 3, hx - 1, hy + 4, Part::Eye, pal);
        c.rect(hx, hy + 4, hx + 1, hy + 4, Part::Eye, pal);
        c.rect(hx + 1, hy + 3, hx + 1, hy + 3, Part::Eye, pal);
    }

    if p.blush {
        c.ellipse(hx - 7, hy + 2, 2, 1, Part::Blush, pal);
        c.ellipse(hx + 7, hy + 2, 2, 1, Part::Blush, pal);
    }

    // 前爪
    if p.lying {
        if p.paw_left == Paw::Up { c.ellipse(13, 27 + bdy, 2, 2, Part::Body, pal); }
        if p.paw_right == Paw::Up { c.ellipse(19, 27 + bdy, 2, 2, Part::Body, pal); }
    } else {
        let base_y = 29 + bdy;
        if p.paw_left == Paw::Down {
            c.ellipse(12 + bdx, base_y, 3, 2, Part::Body, pal);
        } else {
            c.ellipse(8 + bdx, 20 + bdy, 2, 3, Part::Body, pal);
            c.ellipse(8 + bdx, 17 + bdy, 2, 2, Part::Pink, pal);
        }
        if p.paw_right == Paw::Down {
            c.ellipse(20 + bdx, base_y, 3, 2, Part::Body, pal);
        } else {
            c.ellipse(24 + bdx, 20 + bdy, 2, 3, Part::Body, pal);
            c.ellipse(24 + bdx, 17 + bdy, 2, 2, Part::Pink, pal);
        }
    }

    if p.sparkle {
        for (sx, sy) in [(6, 4), (26, 6)] {
            c.rect(sx, sy, sx, sy + 1, Part::Gold, pal);
            c.rect(sx - 1, sy, sx + 1, sy, Part::Gold, pal);
        }
    }
    if p.heart {
        for (hxp, hyp) in [(26, 3), (23, 6)] {
            c.rect(hxp, hyp, hxp + 1, hyp + 2, Part::Pink, pal);
            c.rect(hxp + 2, hyp, hxp + 3, hyp + 2, Part::Pink, pal);
            c.rect(hxp + 1, hyp + 1, hxp + 2, hyp + 3, Part::Pink, pal);
        }
    }
    if p.zzz {
        for (zx, zy) in [(25, 3), (28, 6)] {
            c.rect(zx, zy, zx + 2, zy, Part::Eye, pal);
            c.rect(zx + 2, zy + 1, zx + 2, zy + 1, Part::Eye, pal);
            c.rect(zx, zy + 2, zx + 2, zy + 2, Part::Eye, pal);
        }
    }

    c.outline_pass(pal);
}

/// 钉选表情（pose.expr>0 时覆盖自动眼睛/配饰）
fn apply_expr(p: &mut CatPose, expr: usize) {
    match expr {
        1 => { p.eye = Eye::Happy; }
        2 => { p.eye = Eye::Shock; p.mouth_open = true; }
        3 => { p.eye = Eye::Closed; }
        4 => { p.eye = Eye::Star; }
        5 => { p.eye = Eye::Heart; }
        6 => { p.eye = Eye::Wink; }
        7 => { p.blush = true; }
        8 => { p.mouth_open = true; p.tongue = true; }
        _ => {}
    }
}

/// 主入口：渲染当前 Pose 到 512×512 缓冲
pub fn render_into(out: &mut Vec<u32>, pose: &Pose, pal: &Palette, expr: usize) {
    let mut c = Canvas::new();
    let mut p = CatPose::default();
    let t = pose.tick;

    match pose.state {
        PetState::Idle | PetState::Walk => {
            // 走路：身体摆动 + 尾巴上翘；待机：偶尔眨眼
            if pose.state == PetState::Walk {
                p.body_dy = if t % 2 == 0 { -1 } else { 1 };
                p.tail = Tail::Up;
                p.head_dy = if t % 4 < 2 { 0 } else { 1 };
            } else {
                p.tail = Tail::Right;
            }
            if expr > 0 {
                apply_expr(&mut p, expr);
            } else if pose.typing || (pose.state == PetState::Idle && t % 8 == 7) {
                p.eye = Eye::Closed;
            }
        }
        PetState::Sleep => {
            p.eye = Eye::Closed;
            p.lying = false;
            p.tail = Tail::Wrap;
            p.zzz = t % 4 < 2;
            p.body_dy = if t % 4 < 2 { 0 } else { 1 };
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Closed; }
        }
        PetState::Patted => {
            p.eye = Eye::Happy;
            p.tail = Tail::Up;
            p.heart = true;
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Happy; }
        }
        PetState::Shocked | PetState::Thrown | PetState::Startle => {
            p.eye = Eye::Shock;
            p.mouth_open = true;
            p.tail = Tail::Out;
            p.body_dy = if pose.state == PetState::Startle { -2 } else { 0 };
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Shock; }
        }
        PetState::Dragged => {
            p.eye = Eye::Shock;
            p.mouth_open = true;
            p.tail = Tail::Out;
            p.paw_left = Paw::Up;
            p.paw_right = Paw::Up;
        }
        PetState::Climb => {
            // 爬墙姿态（横向窗口内旋转 90° 由窗口外层处理：这里画"向上爬"）
            p.tail = Tail::Up;
            p.paw_left = if t % 2 == 0 { Paw::Up } else { Paw::Down };
            p.paw_right = if t % 2 == 0 { Paw::Down } else { Paw::Up };
            p.body_dy = if t % 2 == 0 { 0 } else { -1 };
            if expr > 0 { apply_expr(&mut p, expr); }
        }
        PetState::Sitting | PetState::Perch => {
            p.tail = Tail::Wrap;
            p.eye = if t % 8 == 7 { Eye::Closed } else { Eye::Open };
            if expr > 0 { apply_expr(&mut p, expr); }
        }
        PetState::Stretch => {
            p.body_dy = 2;
            p.head_dy = 2;
            p.eye = Eye::Closed;
            p.tail = Tail::Up;
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Closed; }
        }
        PetState::Groom => {
            p.paw_left = Paw::Up;
            p.eye = Eye::Closed;
            p.mouth_open = t % 2 == 0;
            p.tongue = t % 2 == 0;
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Closed; }
        }
        PetState::Eat => {
            p.head_dy = 2;
            p.mouth_open = t % 2 == 0;
            p.ball = true; // 碗
            if expr > 0 { apply_expr(&mut p, expr); }
        }
        PetState::Roll => {
            p.lying = true;
            p.eye = Eye::Happy;
            p.body_dx = match t % 4 { 0 => -5, 1 => -2, 2 => 4, _ => 1 };
            p.tail = Tail::Out;
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Happy; }
        }
        PetState::PlayBall => {
            p.eye = Eye::Happy;
            p.ball = t % 2 == 0;
            p.paw_right = if t % 2 == 0 { Paw::Up } else { Paw::Down };
            p.body_dy = if t % 2 == 0 { 0 } else { -1 };
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Happy; }
        }
        PetState::TailChase => {
            p.eye = Eye::Happy;
            p.tail = match t % 3 {
                0 => Tail::Right,
                1 => Tail::Wrap,
                _ => Tail::Up,
            };
            p.body_dy = if t % 2 == 0 { 0 } else { -1 };
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Happy; }
        }
        PetState::Wave => {
            p.eye = Eye::Happy;
            p.paw_right = Paw::Up;
            p.sparkle = t % 2 == 0;
            p.tail = Tail::Up;
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Happy; }
        }
        PetState::Purr => {
            p.eye = Eye::Closed;
            p.heart = t % 2 == 0;
            p.body_dy = if t % 2 == 0 { 0 } else { 1 };
            p.tail = Tail::Wrap;
            if expr > 0 { apply_expr(&mut p, expr); } else { p.eye = Eye::Closed; }
        }
    }

    draw_cat(&mut c, &p, pal);
    // 逻辑格 → 512 物理缓冲：逻辑像素值即调色板颜色（渲染时已着色），
    // 这里按 SCALE 放大输出
    out.clear();
    out.reserve((SIDE * SIDE) as usize);
    for y in 0..SIDE {
        let gy = (y / SCALE) as usize;
        for x in 0..SIDE {
            let gx = (x / SCALE) as usize;
            let v = c.px[gy * GRID as usize + gx];
            out.push(if v == T as u32 { 0 } else { 0xFF000000 | v });
        }
    }
}

#[cfg(test)]
mod tmp_visual {
    use super::*;
    use crate::model::Pose;

    fn pose(state: PetState, tick: u32) -> Pose {
        Pose { state, tick, gaze: (0, 0), expr: 0, costume: 0, aux: 0, typing: false }
    }

    fn pal() -> Palette {
        crate::sprites::palettes(crate::sprites::Species::Cat)[0].clone()
    }

    #[test]
    fn tmp_dump_states() {
        let pal = pal();
        let states = [
            PetState::Idle, PetState::Walk, PetState::Sleep, PetState::Patted,
            PetState::Shocked, PetState::Roll, PetState::Wave, PetState::Purr,
            PetState::Eat, PetState::PlayBall,
        ];
        let cols = 5;
        let cell = 128 + 6;
        let iw = cell * cols + 6;
        let ih = cell * 2 + 6;
        let mut img = vec![0xFFB0B8C4u32; (iw * ih) as usize];
        for (i, st) in states.iter().enumerate() {
            let mut out = Vec::new();
            render_into(&mut out, &pose(*st, 1), &pal, 0);
            // 512 -> 128 下采样
            let (cx, cy) = ((i % cols) * cell + 3, (i / cols) * cell + 3);
            for y in 0..128 {
                for x in 0..128 {
                    let v = out[(y * 4) * 512 + x * 4];
                    img[((cy + y) as usize) * iw + (cx + x) as usize] = if v == 0 { 0xFFB0B8C4 } else { 0xFF000000 | v };
                }
            }
        }
        let mut rgba = Vec::with_capacity((iw * ih * 4) as usize);
        for &p in &img {
            rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 0xFF]);
        }
        let file = std::fs::File::create("/tmp/cat32_states.png").unwrap();
        let mut enc = png::Encoder::new(file, iw as u32, ih as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        writer.write_image_data(&rgba).unwrap();
    }
}
