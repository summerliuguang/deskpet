//! 自绘窗口共享的像素绘图原语。
//!
//! 圆角内含判定、半透明面板、alpha 混色此前在 menu.rs 与 bubble.rs
//! 各有一份近似拷贝，这里收敛为单一实现；绘制风格改动只动这一处。

/// 颜色按 src 的 alpha 通道混合到 dst（两色均 0xAARRGGBB，返回不透明）
pub fn blend(dst: u32, src: u32) -> u32 {
    let pa = (src >> 24) & 0xFF;
    let mix = |sc: u32, dc: u32| -> u32 { (sc * pa + dc * (255 - pa)) / 255 };
    0xFF000000
        | (mix((src >> 16) & 0xFF, (dst >> 16) & 0xFF) << 16)
        | (mix((src >> 8) & 0xFF, (dst >> 8) & 0xFF) << 8)
        | mix(src & 0xFF, dst & 0xFF)
}

/// 圆角矩形内含判定：四角为半径 r 的 1/4 圆内。
/// y 从 0 起（局部坐标），调用方负责把窗口内的横带平移到 0。
pub fn rounded_inside(x: i32, y: i32, w: i32, h: i32, r: i32) -> bool {
    if x < 0 || y < 0 || x >= w || y >= h {
        return false;
    }
    let (rx, ry) = (w - 1 - x, h - 1 - y);
    for (cx, cy) in [(r, r), (rx, ry), (r, ry), (rx, r)] {
        if cx < r && cy < r {
            let (dx, dy) = (cx - r, cy - r);
            if dx * dx + dy * dy > r * r {
                return false;
            }
        }
    }
    true
}

/// 半透明底 + 1px 内描边的圆角面板（右键菜单底板同款画法）。
/// bg 需带 alpha（如 0xCD101014），border 为不透明色。
pub fn rounded_panel(buf: &mut [u32], w: i32, h: i32, radius: i32, bg: u32, border: u32) {
    let inside = |x: i32, y: i32| rounded_inside(x, y, w, h, radius);
    for y in 0..h {
        for x in 0..w {
            if inside(x, y) {
                buf[(y * w + x) as usize] = bg;
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            if inside(x, y) {
                let neigh_out = !inside(x - 1, y)
                    || !inside(x + 1, y)
                    || !inside(x, y - 1)
                    || !inside(x, y + 1);
                if neigh_out {
                    buf[(y * w + x) as usize] = border;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_inside_rejects_corners_and_outside() {
        let (w, h, r) = (10, 10, 3);
        assert!(rounded_inside(5, 5, w, h, r), "中心点在内");
        assert!(!rounded_inside(0, 0, w, h, r), "直角点被圆角剔除");
        assert!(!rounded_inside(-1, 5, w, h, r) && !rounded_inside(5, -1, w, h, r));
        assert!(!rounded_inside(10, 5, w, h, r) && !rounded_inside(5, 10, w, h, r));
        // 圆角内的点：(2,2) 距角心 (3,3) 距离平方 = 2 < 9
        assert!(rounded_inside(2, 2, w, h, r));
    }

    #[test]
    fn blend_respects_src_alpha() {
        let dst = 0xFF88CCFF;
        assert_eq!(blend(dst, 0xFF000000), 0xFF000000, "全不透明 src 完全覆盖");
        assert_eq!(blend(dst, 0x00FFFFFF), dst, "全透明 src 不改变底色");
        let mid = blend(dst, 0x80FFFFFF);
        let r = (mid >> 16) & 0xFF;
        assert!(r > 0x88 && r < 0xFF, "半透明白应提升红色通道: {mid:08X}");
    }
}
