//! Composition of a window's layers into its one Wayland buffer, the part SurfaceFlinger plays on
//! Android. A window's own pixels are the base; a child layer, such as a `SurfaceView`'s, is drawn
//! below it, showing where the window left its pixels transparent, or above it.
//!
//! Pixels are 0xAARRGGBB without premultiplied alpha, as `Bitmap.getPixels` gives them. The result
//! is opaque, because the window's buffer is XRGB8888.

/// A layer's pixels and where they go in the window.
pub struct Placed<'a> {
    pub pixels: &'a [u32],
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
}

const OPAQUE: u32 = 0xff00_0000;

/// `source` over the opaque `destination`.
fn over(source: u32, destination: u32) -> u32 {
    let alpha = source >> 24;
    match alpha {
        255 => source | OPAQUE,
        0 => destination,
        _ => {
            let blend = |shift: u32| {
                let s = (source >> shift) & 0xff;
                let d = (destination >> shift) & 0xff;
                ((s * alpha + d * (255 - alpha) + 127) / 255) << shift
            };
            OPAQUE | blend(16) | blend(8) | blend(0)
        }
    }
}

/// Blends `layer` over `frame`, clipped to the frame. Sizes that do not match their pixels draw
/// only the rows the pixels hold.
fn draw(frame: &mut [u32], width: i32, height: i32, layer: &Placed<'_>) {
    if layer.width <= 0 || layer.height <= 0 {
        return;
    }
    let left = layer.x.max(0);
    let right = layer.x.saturating_add(layer.width).min(width);
    let top = layer.y.max(0);
    let bottom = layer.y.saturating_add(layer.height).min(height);
    for row in top..bottom {
        for column in left..right {
            let from =
                ((row - layer.y) as usize) * layer.width as usize + (column - layer.x) as usize;
            let to = row as usize * width as usize + column as usize;
            if let (Some(&source), Some(target)) = (layer.pixels.get(from), frame.get_mut(to)) {
                *target = over(source, *target);
            }
        }
    }
}

/// Fills `frame`, `width` by `height`, with the layers in order: `below`, then `base` at the
/// origin, then `above`. Uncovered pixels are black.
pub fn compose(
    frame: &mut [u32],
    width: i32,
    height: i32,
    base: &Placed<'_>,
    below: &[Placed<'_>],
    above: &[Placed<'_>],
) {
    frame.fill(OPAQUE);
    for layer in below.iter().chain(std::iter::once(base)).chain(above) {
        draw(frame, width, height, layer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: u32 = 0xffff_0000;
    const BLUE: u32 = 0xff00_00ff;
    const CLEAR: u32 = 0x0000_0000;

    fn placed(pixels: &[u32], width: i32, height: i32, x: i32, y: i32) -> Placed<'_> {
        Placed {
            pixels,
            width,
            height,
            x,
            y,
        }
    }

    #[test]
    fn a_layer_below_shows_only_where_the_window_is_transparent() {
        // A 2x1 window: opaque red on the left, a punched hole on the right.
        let base = [RED, CLEAR];
        let child = [BLUE, BLUE];
        let mut frame = [0; 2];

        compose(
            &mut frame,
            2,
            1,
            &placed(&base, 2, 1, 0, 0),
            &[placed(&child, 2, 1, 0, 0)],
            &[],
        );

        assert_eq!(frame, [RED, BLUE]);
    }

    #[test]
    fn a_layer_above_covers_the_window() {
        let base = [RED, RED];
        let child = [BLUE];
        let mut frame = [0; 2];

        compose(
            &mut frame,
            2,
            1,
            &placed(&base, 2, 1, 0, 0),
            &[],
            &[placed(&child, 1, 1, 1, 0)],
        );

        assert_eq!(frame, [RED, BLUE]);
    }

    #[test]
    fn translucent_pixels_blend_and_the_result_is_opaque() {
        let base = [0x80ff_ffff];
        let mut frame = [0; 1];

        compose(&mut frame, 1, 1, &placed(&base, 1, 1, 0, 0), &[], &[]);

        assert_eq!(frame, [0xff80_8080]);
    }

    #[test]
    fn layers_are_clipped_to_the_window_and_to_their_pixels() {
        let base = [CLEAR; 4];
        // Placed half outside, and claiming more rows than it has pixels for.
        let child = [BLUE, BLUE, BLUE, BLUE];
        let mut frame = [0; 4];

        compose(
            &mut frame,
            2,
            2,
            &placed(&base, 2, 2, 0, 0),
            &[placed(&child, 2, 3, 1, -1)],
            &[],
        );

        assert_eq!(frame, [OPAQUE, BLUE, OPAQUE, OPAQUE]);
    }

    #[test]
    fn an_empty_layer_draws_nothing() {
        let base = [RED];
        let mut frame = [0; 1];

        compose(
            &mut frame,
            1,
            1,
            &placed(&base, 1, 1, 0, 0),
            &[placed(&[], 0, 0, 0, 0)],
            &[placed(&[], -3, 5, 0, 0)],
        );

        assert_eq!(frame, [RED]);
    }
}
