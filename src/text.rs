//! 文本渲染：fontdue + 内置 Fusion Pixel 12px（缝合像素字体，SIL OFL 1.1 可商用，
//! 简中全量覆盖）。直接往 softbuffer 的 ARGB 缓冲里混合像素，供气泡和输入框使用。
//!
//! Fusion Pixel 按 12px 设计，渲染尺寸必须是 12 的整数倍才保持像素锐利。

use fontdue::{Font, FontSettings};
use std::sync::LazyLock;

const FONT_BYTES: &[u8] = include_bytes!("../assets/fusion-pixel-12px-zh_hans.otf");

/// 界面基准字号：12px 像素字体 × UI 缩放（四舍五入到整数像素）
pub fn base_px() -> i32 {
    ((12.0f64) * crate::ui_scale()).round() as i32
}

pub static TEXT: LazyLock<TextEngine> = LazyLock::new(TextEngine::new);

pub use crate::ui;

pub struct TextEngine {
    font: Font,
}

pub type Rgb = (u8, u8, u8);

/// 裁剪区域 (x0, y0, x1, y1)，超出部分不画
pub type Clip = (i32, i32, i32, i32);

const FULL_CLIP: Clip = (i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2);

pub struct Layout {
    pub lines: Vec<String>,
    pub width: i32,
    pub height: i32,
}

impl TextEngine {
    fn new() -> Self {
        let font = Font::from_bytes(
            FONT_BYTES,
            FontSettings { collection_index: 0, ..Default::default() },
        )
        .expect("内置字体解析失败");
        Self { font }
    }

    pub fn line_height(&self, px: i32) -> i32 {
        match self.font.horizontal_line_metrics(px as f32) {
            Some(m) => (m.ascent - m.descent + m.line_gap).round() as i32,
            None => px + 2,
        }
    }

    pub fn text_width(&self, s: &str, px: i32) -> i32 {
        s.chars()
            .map(|c| self.font.metrics(c, px as f32).advance_width)
            .sum::<f32>() as i32
    }

    /// 贪心逐字符折行（中文逐字断行，ASCII 连续串尽量不拆）
    pub fn wrap(&self, s: &str, max_w: i32, px: i32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut cur = String::new();
        let mut w = 0.0f32;
        for ch in s.chars() {
            if ch == '\n' {
                lines.push(std::mem::take(&mut cur));
                w = 0.0;
                continue;
            }
            let adv = self.font.metrics(ch, px as f32).advance_width;
            if w + adv > max_w as f32 && !cur.is_empty() {
                // ASCII 单词尽量整体换行
                if ch.is_ascii_graphic() {
                    if let Some(pos) = cur.rfind(' ') {
                        let tail = cur.split_off(pos + 1);
                        lines.push(std::mem::take(&mut cur));
                        cur = tail;
                        w = self.text_width(&cur, px) as f32;
                    }
                } else {
                    lines.push(std::mem::take(&mut cur));
                    w = 0.0;
                }
            }
            cur.push(ch);
            w += adv;
        }
        lines.push(cur);
        lines
    }

    pub fn layout(&self, s: &str, max_w: i32, px: i32) -> Layout {
        let lines = self.wrap(s, max_w, px);
        let width = lines.iter().map(|l| self.text_width(l, px)).max().unwrap_or(0);
        let height = lines.len() as i32 * self.line_height(px);
        Layout { lines, width, height }
    }

    /// 在 baseline=(x, y) 处画一行文本
    fn draw_line(
        &self,
        buf: &mut [u32],
        bw: i32,
        bh: i32,
        clip: Clip,
        px: i32,
        x: i32,
        y: i32,
        s: &str,
        color: Rgb,
    ) {
        let mut pen_x = x;
        for ch in s.chars() {
            let (m, bitmap) = self.font.rasterize(ch, px as f32);
            let gx = pen_x + m.xmin;
            let gy = y + m.ymin;
            if !bitmap.is_empty() {
                for row in 0..m.height as i32 {
                    let py = gy + row;
                    if py < clip.1 || py > clip.3 || py < 0 || py >= bh {
                        continue;
                    }
                    for col in 0..m.width as i32 {
                        let px = gx + col;
                        if px < clip.0 || px > clip.2 || px < 0 || px >= bw {
                            continue;
                        }
                        let cov = bitmap[(row * m.width as i32 + col) as usize] as u32;
                        if cov == 0 {
                            continue;
                        }
                        let idx = (py * bw + px) as usize;
                        let dst = buf[idx];
                        let (er, eg, eb) = ((dst >> 16) & 0xFF, (dst >> 8) & 0xFF, dst & 0xFF);
                        let mix = |ec: u32, c: u8| -> u32 {
                            (ec * (255 - cov) + (c as u32) * cov) / 255
                        };
                        buf[idx] = 0xFF000000
                            | (mix(er, color.0) << 16)
                            | (mix(eg, color.1) << 8)
                            | mix(eb, color.2);
                    }
                }
            }
            pen_x += m.advance_width as i32;
        }
    }

    /// 多行绘制。x 为左边缘（或右对齐时的右边缘），y 为首行顶部。
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        buf: &mut [u32],
        bw: i32,
        bh: i32,
        px: i32,
        x: i32,
        y: i32,
        lines: &[String],
        color: Rgb,
        align_right: bool,
    ) {
        self.draw_clipped(buf, bw, bh, FULL_CLIP, px, x, y, lines, color, align_right);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_clipped(
        &self,
        buf: &mut [u32],
        bw: i32,
        bh: i32,
        clip: Clip,
        px: i32,
        x: i32,
        y: i32,
        lines: &[String],
        color: Rgb,
        align_right: bool,
    ) {
        let lh = self.line_height(px);
        for (i, line) in lines.iter().enumerate() {
            let lx = if align_right { x - self.text_width(line, px) } else { x };
            let baseline = y + (i as i32) * lh + lh - 3;
            self.draw_line(buf, bw, bh, clip, px, lx, baseline, line, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_parses_and_line_height_sane() {
        let lh = TEXT.line_height(12);
        assert!(lh >= 12 && lh <= 20, "fusion-pixel 12px 行高异常: {lh}");
    }

    #[test]
    fn chinese_glyphs_render_pixels() {
        // 关键验证：内置 OTF 能被 fontdue 解析，中文字形真实落在缓冲里
        let mut buf = vec![0xFF88CCFF; 200 * 40];
        TEXT.draw(&mut buf, 200, 40, 12, 2, 2, &["喵呜 Hello 123".to_string()], (0, 0, 0), false);
        assert!(buf.iter().any(|&p| p != 0xFF88CCFF), "中英文混排没有画出任何字形");
    }

    #[test]
    fn wrap_breaks_long_cjk_lines() {
        let lines = TEXT.wrap("一二三四五六七八九十甲乙丙丁戊己庚辛", 72, 12);
        assert!(lines.len() >= 2, "长中文应折行");
        assert!(lines.iter().all(|l| TEXT.text_width(l, 12) <= 72));
    }
}
