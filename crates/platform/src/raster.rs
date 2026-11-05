//! Software drawing into 32-bit XRGB pixels. Safe code only.

use font8x8::UnicodeFonts;

const GLYPH_SIZE: f32 = 8.0;

pub struct Frame<'a> {
    pub pixels: &'a mut [u32],
    pub width: i32,
    pub height: i32,
}

fn blend(destination: u32, color: u32) -> u32 {
    let alpha = color >> 24;
    if alpha == 255 {
        return color;
    }
    let mix = |shift: u32| {
        let source = (color >> shift) & 0xFF;
        let target = (destination >> shift) & 0xFF;
        ((source * alpha + target * (255 - alpha)) / 255) << shift
    };
    0xFF00_0000 | mix(16) | mix(8) | mix(0)
}

/// Clips to the frame, so any coordinates from an app are safe. NaN clips to nothing.
pub fn fill_rect(frame: &mut Frame, left: f32, top: f32, right: f32, bottom: f32, color: u32) {
    let clamp = |value: f32, max: i32| (value.round() as i32).clamp(0, max);
    let (x0, x1) = (clamp(left, frame.width), clamp(right, frame.width));
    let (y0, y1) = (clamp(top, frame.height), clamp(bottom, frame.height));
    if x0 >= x1 {
        return;
    }
    for y in y0..y1 {
        let row = (y * frame.width) as usize;
        let span = &mut frame.pixels[row + x0 as usize..row + x1 as usize];
        if color >> 24 == 255 {
            span.fill(color);
        } else {
            for pixel in span {
                *pixel = blend(*pixel, color);
            }
        }
    }
}

/// `y` is the baseline, as on Android. Characters outside the font are skipped.
pub fn draw_text(frame: &mut Frame, text: &str, x: f32, y: f32, color: u32, size: f32) {
    let scale = (size / GLYPH_SIZE).round().max(1.0);
    let mut pen = x;
    for character in text.chars() {
        if let Some(glyph) = font8x8::BASIC_FONTS.get(character) {
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..8 {
                    if bits & (1 << column) != 0 {
                        let left = pen + column as f32 * scale;
                        let top = y + (row as f32 - GLYPH_SIZE) * scale;
                        fill_rect(frame, left, top, left + scale, top + scale, color);
                    }
                }
            }
        }
        pen += GLYPH_SIZE * scale;
    }
}
