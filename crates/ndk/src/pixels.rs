//! Conversions between the framework's color ints and the pixel memory layouts of the NDK.
//!
//! Pixels cross into Java as `Bitmap.getPixels` and `setPixels` define them: unpremultiplied
//! 0xAARRGGBB ints, whatever the host Skia stores. The host graphics library cannot take NIO
//! buffers on ART, as it looks up OpenJDK's `FileDescriptor.fd`, so int arrays are the only route.
//! In memory, Android keeps `RGBA_8888` as R, G, B, A bytes, premultiplied when the Bitmap is, and
//! `RGB_565` as little-endian 16-bit words.

fn premultiply(channel: u32, alpha: u32) -> u8 {
    ((channel * alpha + 127) / 255) as u8
}

fn unpremultiply(channel: u8, alpha: u8) -> u32 {
    match alpha {
        0 => 0,
        255 => u32::from(channel),
        alpha => ((u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255),
    }
}

/// Writes `width` colors per row into `memory`, whose rows are `stride` bytes apart.
pub fn colors_to_memory(
    colors: &[i32],
    width: usize,
    memory: &mut [u8],
    stride: usize,
    rgb565: bool,
    premultiplied: bool,
) {
    if width == 0 {
        return;
    }
    for (row, colors) in memory.chunks_mut(stride).zip(colors.chunks(width)) {
        if rgb565 {
            for (pixel, &color) in row.as_chunks_mut::<2>().0.iter_mut().zip(colors) {
                let color = color as u32;
                let (r, g, b) = ((color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF);
                *pixel = (((r >> 3) << 11 | (g >> 2) << 5 | b >> 3) as u16).to_le_bytes();
            }
        } else {
            for (pixel, &color) in row.as_chunks_mut::<4>().0.iter_mut().zip(colors) {
                let color = color as u32;
                let alpha = color >> 24;
                let channel = |shift: u32| {
                    let value = (color >> shift) & 0xFF;
                    if premultiplied {
                        premultiply(value, alpha)
                    } else {
                        value as u8
                    }
                };
                *pixel = [channel(16), channel(8), channel(0), alpha as u8];
            }
        }
    }
}

/// Reads `width` pixels per row from `memory` into colors; `opaque` ignores the alpha bytes.
pub fn memory_to_colors(
    memory: &[u8],
    stride: usize,
    colors: &mut [i32],
    width: usize,
    rgb565: bool,
    premultiplied: bool,
    opaque: bool,
) {
    if width == 0 {
        return;
    }
    for (row, colors) in memory.chunks(stride).zip(colors.chunks_mut(width)) {
        if rgb565 {
            for (pixel, color) in row.as_chunks::<2>().0.iter().zip(colors) {
                let word = u32::from(u16::from_le_bytes(*pixel));
                let (r, g, b) = ((word >> 11) & 0x1F, (word >> 5) & 0x3F, word & 0x1F);
                let (r, g, b) = (r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2);
                *color = (0xFF00_0000 | r << 16 | g << 8 | b) as i32;
            }
        } else {
            for (pixel, color) in row.as_chunks::<4>().0.iter().zip(colors) {
                let [r, g, b, a] = *pixel;
                let a = if opaque { 255 } else { a };
                let channel = |value: u8| {
                    if premultiplied {
                        unpremultiply(value, a)
                    } else {
                        u32::from(value)
                    }
                };
                *color =
                    (u32::from(a) << 24 | channel(r) << 16 | channel(g) << 8 | channel(b)) as i32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_colors_round_trip_through_rgba_memory() {
        let colors = [0xFF102030u32 as i32, 0xFF405060u32 as i32];
        let mut memory = [0u8; 12];
        colors_to_memory(&colors, 2, &mut memory, 12, false, true);
        assert_eq!(
            &memory[..8],
            &[0x10, 0x20, 0x30, 0xFF, 0x40, 0x50, 0x60, 0xFF]
        );
        let mut back = [0; 2];
        memory_to_colors(&memory, 12, &mut back, 2, false, true, false);
        assert_eq!(back, colors);
    }

    #[test]
    fn premultiplied_memory_scales_by_alpha() {
        let mut memory = [0u8; 4];
        colors_to_memory(&[0x80FF0000u32 as i32], 1, &mut memory, 4, false, true);
        assert_eq!(memory, [0x80, 0, 0, 0x80]);
        let mut back = [0];
        memory_to_colors(&memory, 4, &mut back, 1, false, true, false);
        assert_eq!(back[0] as u32, 0x80FF0000);
    }

    #[test]
    fn rgbx_memory_reads_as_opaque_and_565_keeps_its_extremes() {
        let mut colors = [0];
        memory_to_colors(&[1, 2, 3, 0], 4, &mut colors, 1, false, false, true);
        assert_eq!(colors[0] as u32, 0xFF010203);
        let mut memory = [0u8; 4];
        colors_to_memory(
            &[0xFFFFFFFFu32 as i32, 0xFF000000u32 as i32],
            2,
            &mut memory,
            4,
            true,
            false,
        );
        assert_eq!(memory, [0xFF, 0xFF, 0, 0]);
        let mut back = [0; 2];
        memory_to_colors(&memory, 4, &mut back, 2, true, false, true);
        assert_eq!(back.map(|c| c as u32), [0xFFFFFFFF, 0xFF000000]);
    }
}
