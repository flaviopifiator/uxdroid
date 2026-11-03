//! Composition of a window's layers into its one Wayland buffer, the pixel half of what
//! SurfaceFlinger does on Android: each layer's frame drawn in order, through its transform, buffer
//! transform, clip, alpha and rounded corners, and only inside the damaged part of the window.
//!
//! Pixels are 0xAARRGGBB without premultiplied alpha, as `Bitmap.getPixels` gives them. The result
//! is opaque, because the window's buffer is XRGB8888. Sampling is nearest-neighbour.

/// A rectangle of whole pixels, right and bottom exclusive. Empty when either side is not positive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const EMPTY: Rect = Rect::new(0, 0, 0, 0);
    /// Everything: a clip that clips nothing.
    #[cfg(test)]
    pub const ALL: Rect = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);

    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn sized(width: i32, height: i32) -> Rect {
        Rect::new(0, 0, width, height)
    }

    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let rect = Rect::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        );
        if rect.is_empty() { Rect::EMPTY } else { rect }
    }

    /// The smallest rectangle holding both; an empty one adds nothing.
    pub fn union(&self, other: &Rect) -> Rect {
        match (self.is_empty(), other.is_empty()) {
            (true, _) => *other,
            (_, true) => *self,
            _ => Rect::new(
                self.left.min(other.left),
                self.top.min(other.top),
                self.right.max(other.right),
                self.bottom.max(other.bottom),
            ),
        }
    }

    pub fn width(&self) -> i32 {
        self.right.saturating_sub(self.left).max(0)
    }

    pub fn height(&self) -> i32 {
        self.bottom.saturating_sub(self.top).max(0)
    }
}

/// An affine map from a layer's space to its parent's, as SurfaceFlinger's: `x' = a x + c y + tx`,
/// `y' = b x + d y + ty`. `SurfaceControl.setMatrix(dsdx, dtdx, dtdy, dsdy)` gives a, b, c, d.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    /// `matrix` placed at `(x, y)`: what a layer's position and matrix make together.
    pub fn placed(x: f32, y: f32, matrix: [f32; 4]) -> Affine {
        let [a, b, c, d] = matrix;
        Affine {
            a,
            b,
            c,
            d,
            tx: x,
            ty: y,
        }
    }

    /// This map after `inner`: a point goes through `inner` first.
    pub fn then(&self, inner: &Affine) -> Affine {
        Affine {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            tx: self.a * inner.tx + self.c * inner.ty + self.tx,
            ty: self.b * inner.tx + self.d * inner.ty + self.ty,
        }
    }

    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.tx,
            self.b * x + self.d * y + self.ty,
        )
    }

    pub fn inverse(&self) -> Option<Affine> {
        let determinant = self.a * self.d - self.b * self.c;
        if determinant == 0.0 || !determinant.is_finite() {
            return None;
        }
        let (a, b, c, d) = (
            self.d / determinant,
            -self.b / determinant,
            -self.c / determinant,
            self.a / determinant,
        );
        Some(Affine {
            a,
            b,
            c,
            d,
            tx: -(a * self.tx + c * self.ty),
            ty: -(b * self.tx + d * self.ty),
        })
    }

    /// Whether this only moves by whole pixels, which draws by copying rows.
    fn is_pixel_translation(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.tx.fract() == 0.0
            && self.ty.fract() == 0.0
    }

    /// Whether rectangles stay rectangles with their sides on the axes.
    pub fn keeps_axes(&self) -> bool {
        (self.b == 0.0 && self.c == 0.0) || (self.a == 0.0 && self.d == 0.0)
    }

    /// The whole pixels covering the rectangle `(left, top, right, bottom)` once mapped, saturated
    /// to `i32`.
    pub fn bounds(&self, rect: [f32; 4]) -> Rect {
        let [left, top, right, bottom] = rect;
        let corners = [
            self.map(left, top),
            self.map(right, top),
            self.map(left, bottom),
            self.map(right, bottom),
        ];
        let fold = |pick: fn(&(f32, f32)) -> f32, min: bool| {
            corners.iter().map(pick).fold(
                if min {
                    f32::INFINITY
                } else {
                    f32::NEG_INFINITY
                },
                |a, b| {
                    if min { a.min(b) } else { a.max(b) }
                },
            )
        };
        // `as` saturates, and NaN becomes 0.
        let rect = Rect::new(
            fold(|p| p.0, true).floor() as i32,
            fold(|p| p.1, true).floor() as i32,
            fold(|p| p.0, false).ceil() as i32,
            fold(|p| p.1, false).ceil() as i32,
        );
        if rect.is_empty() { Rect::EMPTY } else { rect }
    }
}

/// `SurfaceControl.setBufferTransform` flags, as Android's `NATIVE_WINDOW_TRANSFORM_*`: the flips
/// apply first, then the quarter turn clockwise.
pub const FLIP_H: u32 = 1;
pub const FLIP_V: u32 = 2;
pub const ROT_90: u32 = 4;

/// The size a buffer of `width` by `height` takes in its layer after `transform`.
pub fn transformed_size(width: i32, height: i32, transform: u32) -> (i32, i32) {
    if transform & ROT_90 != 0 {
        (height, width)
    } else {
        (width, height)
    }
}

/// The buffer pixel shown at pixel `(u, v)` of the layer, for a buffer of `width` by `height`.
fn buffer_pixel(u: i32, v: i32, width: i32, height: i32, transform: u32) -> (i32, i32) {
    let (mut x, mut y) = if transform & ROT_90 != 0 {
        (v, height - 1 - u)
    } else {
        (u, v)
    };
    if transform & FLIP_H != 0 {
        x = width - 1 - x;
    }
    if transform & FLIP_V != 0 {
        y = height - 1 - y;
    }
    (x, y)
}

/// The rectangle of the layer showing the buffer rectangle `rect` after `transform`.
pub fn transformed_rect(rect: Rect, width: i32, height: i32, transform: u32) -> Rect {
    let mut rect = rect;
    if transform & FLIP_H != 0 {
        rect = Rect::new(width - rect.right, rect.top, width - rect.left, rect.bottom);
    }
    if transform & FLIP_V != 0 {
        rect = Rect::new(
            rect.left,
            height - rect.bottom,
            rect.right,
            height - rect.top,
        );
    }
    if transform & ROT_90 != 0 {
        rect = Rect::new(
            height - rect.bottom,
            rect.left,
            height - rect.top,
            rect.right,
        );
    }
    rect
}

/// Rounded corners: pixels of `rect` (window coordinates) farther than `radius` inside a corner
/// are not drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Round {
    pub rect: [f32; 4],
    pub radius: f32,
}

impl Round {
    fn contains(&self, x: f32, y: f32) -> bool {
        let [left, top, right, bottom] = self.rect;
        let radius = self
            .radius
            .min((right - left) / 2.0)
            .min((bottom - top) / 2.0);
        let cx = x.clamp(left + radius, right - radius);
        let cy = y.clamp(top + radius, bottom - radius);
        let (dx, dy) = (x - cx, y - cy);
        dx * dx + dy * dy <= radius * radius
    }
}

/// One layer's frame and how it reaches the window.
pub struct Item<'a> {
    pub pixels: &'a [u32],
    pub width: i32,
    pub height: i32,
    /// From the layer's space, after the buffer transform, to the window.
    pub transform: Affine,
    pub buffer_transform: u32,
    /// Window pixels the layer may cover.
    pub clip: Rect,
    pub alpha: f32,
    pub round: Option<Round>,
}

impl Item<'_> {
    /// The window pixels this item can change.
    pub fn screen(&self) -> Rect {
        let (width, height) = transformed_size(self.width, self.height, self.buffer_transform);
        self.transform
            .bounds([0.0, 0.0, width as f32, height as f32])
            .intersect(&self.clip)
    }
}

const OPAQUE: u32 = 0xff00_0000;

/// `source` with `alpha` over the opaque `destination`.
fn over(source: u32, alpha: u32, destination: u32) -> u32 {
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

/// A pixel's own alpha scaled by the layer's, `scale` out of 256.
fn scaled_alpha(source: u32, scale: u32) -> u32 {
    if scale >= 256 {
        source >> 24
    } else {
        ((source >> 24) * scale + 128) >> 8
    }
}

fn draw(frame: &mut [u32], width: i32, area: Rect, item: &Item<'_>) {
    let target = item.screen().intersect(&area);
    if target.is_empty() || item.width <= 0 || item.height <= 0 || item.alpha <= 0.0 {
        return;
    }
    let scale = (item.alpha.min(1.0) * 256.0).round() as u32;
    if item.buffer_transform == 0 && item.round.is_none() && item.transform.is_pixel_translation() {
        let (x, y) = (item.transform.tx as i32, item.transform.ty as i32);
        for row in target.top..target.bottom {
            let from = (row - y) as usize * item.width as usize;
            let to = row as usize * width as usize;
            for column in target.left..target.right {
                let source = item.pixels.get(from + (column - x) as usize);
                if let (Some(&source), Some(pixel)) = (source, frame.get_mut(to + column as usize))
                {
                    *pixel = over(source, scaled_alpha(source, scale), *pixel);
                }
            }
        }
        return;
    }
    let Some(inverse) = item.transform.inverse() else {
        return;
    };
    let (layer_width, layer_height) =
        transformed_size(item.width, item.height, item.buffer_transform);
    for row in target.top..target.bottom {
        for column in target.left..target.right {
            let (x, y) = (column as f32 + 0.5, row as f32 + 0.5);
            if item.round.is_some_and(|round| !round.contains(x, y)) {
                continue;
            }
            let (u, v) = inverse.map(x, y);
            let (u, v) = (u.floor(), v.floor());
            if u < 0.0 || v < 0.0 || u >= layer_width as f32 || v >= layer_height as f32 {
                continue;
            }
            let (bx, by) = buffer_pixel(
                u as i32,
                v as i32,
                item.width,
                item.height,
                item.buffer_transform,
            );
            let from = by as usize * item.width as usize + bx as usize;
            let to = row as usize * width as usize + column as usize;
            if let (Some(&source), Some(pixel)) = (item.pixels.get(from), frame.get_mut(to)) {
                *pixel = over(source, scaled_alpha(source, scale), *pixel);
            }
        }
    }
}

/// Redraws the `damage` part of `frame`, `width` by `height`, from black and the items in order,
/// bottom first. Pixels outside `damage` keep what they had.
pub fn compose(frame: &mut [u32], width: i32, height: i32, damage: Rect, items: &[Item<'_>]) {
    let area = damage.intersect(&Rect::sized(width, height));
    if area.is_empty() || frame.len() < width as usize * height as usize {
        return;
    }
    for row in area.top..area.bottom {
        let start = row as usize * width as usize;
        frame[start + area.left as usize..start + area.right as usize].fill(OPAQUE);
    }
    for item in items {
        draw(frame, width, area, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: u32 = 0xffff_0000;
    const BLUE: u32 = 0xff00_00ff;
    const GREEN: u32 = 0xff00_ff00;
    const WHITE: u32 = 0xffff_ffff;
    const CLEAR: u32 = 0x0000_0000;

    fn at(pixels: &[u32], width: i32, height: i32, x: f32, y: f32) -> Item<'_> {
        Item {
            pixels,
            width,
            height,
            transform: Affine::placed(x, y, [1.0, 0.0, 0.0, 1.0]),
            buffer_transform: 0,
            clip: Rect::ALL,
            alpha: 1.0,
            round: None,
        }
    }

    fn all(frame: &mut [u32], width: i32, height: i32, items: &[Item<'_>]) {
        compose(frame, width, height, Rect::sized(width, height), items);
    }

    #[test]
    fn a_layer_below_shows_only_where_the_window_is_transparent() {
        // A 2x1 window: opaque red on the left, a punched hole on the right.
        let base = [RED, CLEAR];
        let child = [BLUE, BLUE];
        let mut frame = [0; 2];

        all(
            &mut frame,
            2,
            1,
            &[at(&child, 2, 1, 0.0, 0.0), at(&base, 2, 1, 0.0, 0.0)],
        );

        assert_eq!(frame, [RED, BLUE]);
    }

    #[test]
    fn a_layer_above_covers_the_window() {
        let base = [RED, RED];
        let child = [BLUE];
        let mut frame = [0; 2];

        all(
            &mut frame,
            2,
            1,
            &[at(&base, 2, 1, 0.0, 0.0), at(&child, 1, 1, 1.0, 0.0)],
        );

        assert_eq!(frame, [RED, BLUE]);
    }

    #[test]
    fn translucent_pixels_blend_and_the_result_is_opaque() {
        let base = [0x80ff_ffff];
        let mut frame = [0; 1];

        all(&mut frame, 1, 1, &[at(&base, 1, 1, 0.0, 0.0)]);

        assert_eq!(frame, [0xff80_8080]);
    }

    #[test]
    fn layers_are_clipped_to_the_window_and_to_their_pixels() {
        let base = [CLEAR; 4];
        // Placed half outside, and claiming more rows than it has pixels for.
        let child = [BLUE, BLUE, BLUE, BLUE];
        let mut frame = [0; 4];

        all(
            &mut frame,
            2,
            2,
            &[at(&child, 2, 3, 1.0, -1.0), at(&base, 2, 2, 0.0, 0.0)],
        );

        assert_eq!(frame, [OPAQUE, BLUE, OPAQUE, OPAQUE]);
    }

    #[test]
    fn an_empty_layer_draws_nothing() {
        let base = [RED];
        let mut frame = [0; 1];

        all(
            &mut frame,
            1,
            1,
            &[
                at(&[], 0, 0, 0.0, 0.0),
                at(&base, 1, 1, 0.0, 0.0),
                at(&[], -3, 5, 0.0, 0.0),
            ],
        );

        assert_eq!(frame, [RED]);
    }

    #[test]
    fn layer_alpha_scales_every_pixel() {
        let base = [WHITE];
        let mut item = at(&base, 1, 1, 0.0, 0.0);
        item.alpha = 0.5;
        let mut frame = [0; 1];

        all(&mut frame, 1, 1, &[item]);

        assert_eq!(frame, [0xff80_8080]);
    }

    #[test]
    fn a_matrix_scales_and_moves_the_layer() {
        // One red pixel, doubled and placed at (1, 0) in a 3x2 window.
        let base = [RED];
        let mut item = at(&base, 1, 1, 0.0, 0.0);
        item.transform = Affine::placed(1.0, 0.0, [2.0, 0.0, 0.0, 2.0]);
        let mut frame = [0; 6];

        all(&mut frame, 3, 2, &[item]);

        assert_eq!(frame, [OPAQUE, RED, RED, OPAQUE, RED, RED]);
    }

    #[test]
    fn a_quarter_turn_matrix_rotates_the_layer() {
        // [R B] turned a quarter clockwise around its origin and moved back into view is a column.
        let base = [RED, BLUE];
        let mut item = at(&base, 2, 1, 0.0, 0.0);
        item.transform = Affine::placed(1.0, 0.0, [0.0, 1.0, -1.0, 0.0]);
        let mut frame = [0; 2];

        all(&mut frame, 1, 2, &[item]);

        assert_eq!(frame, [RED, BLUE]);
    }

    #[test]
    fn buffer_transforms_flip_and_turn_the_buffer() {
        let base = [RED, BLUE, GREEN, WHITE]; // 2x2: R B / G W
        let mut frame = [0; 4];
        let mut item = at(&base, 2, 2, 0.0, 0.0);
        item.buffer_transform = FLIP_H;
        all(&mut frame, 2, 2, &[item]);
        assert_eq!(frame, [BLUE, RED, WHITE, GREEN]);

        let mut item = at(&base, 2, 2, 0.0, 0.0);
        item.buffer_transform = FLIP_V;
        all(&mut frame, 2, 2, &[item]);
        assert_eq!(frame, [GREEN, WHITE, RED, BLUE]);

        // A quarter turn clockwise: the left column becomes the top row, read upwards.
        let mut item = at(&base, 2, 2, 0.0, 0.0);
        item.buffer_transform = ROT_90;
        all(&mut frame, 2, 2, &[item]);
        assert_eq!(frame, [GREEN, RED, WHITE, BLUE]);
    }

    #[test]
    fn a_turned_buffer_swaps_its_size_in_the_layer() {
        let base = [RED, BLUE]; // 2x1
        let mut item = at(&base, 2, 1, 0.0, 0.0);
        item.buffer_transform = ROT_90;
        assert_eq!(item.screen(), Rect::new(0, 0, 1, 2));
        let mut frame = [0; 2];

        all(&mut frame, 1, 2, &[item]);

        assert_eq!(frame, [RED, BLUE]);
    }

    #[test]
    fn the_clip_limits_what_a_layer_covers() {
        let base = [RED; 4];
        let mut item = at(&base, 4, 1, 0.0, 0.0);
        item.clip = Rect::new(1, 0, 3, 1);
        let mut frame = [0; 4];

        all(&mut frame, 4, 1, &[item]);

        assert_eq!(frame, [OPAQUE, RED, RED, OPAQUE]);
    }

    #[test]
    fn rounded_corners_cut_the_corner_pixels() {
        let base = [RED; 16];
        let mut item = at(&base, 4, 4, 0.0, 0.0);
        item.round = Some(Round {
            rect: [0.0, 0.0, 4.0, 4.0],
            radius: 2.0,
        });
        let mut frame = [0; 16];

        all(&mut frame, 4, 4, &[item]);

        // The four corner pixels fall outside the rounding; edges and the middle stay.
        for (index, pixel) in frame.iter().enumerate() {
            let corner = matches!(index, 0 | 3 | 12 | 15);
            assert_eq!(*pixel, if corner { OPAQUE } else { RED }, "pixel {index}");
        }
    }

    #[test]
    fn only_the_damage_is_redrawn() {
        let base = [RED; 3];
        let mut frame = [GREEN; 3];

        compose(
            &mut frame,
            3,
            1,
            Rect::new(1, 0, 2, 1),
            &[at(&base, 3, 1, 0.0, 0.0)],
        );

        assert_eq!(frame, [GREEN, RED, GREEN]);
    }

    #[test]
    fn rects_and_maps_combine() {
        let a = Rect::new(0, 0, 2, 2);
        let b = Rect::new(1, 1, 3, 3);
        assert_eq!(a.intersect(&b), Rect::new(1, 1, 2, 2));
        assert_eq!(a.union(&b), Rect::new(0, 0, 3, 3));
        assert_eq!(a.union(&Rect::EMPTY), a);
        assert!(a.intersect(&Rect::new(5, 5, 6, 6)).is_empty());

        let scale = Affine::placed(0.0, 0.0, [2.0, 0.0, 0.0, 3.0]);
        let moved = Affine::placed(10.0, 20.0, [1.0, 0.0, 0.0, 1.0]);
        // The child is scaled first, then moved with its parent.
        assert_eq!(moved.then(&scale).map(1.0, 1.0), (12.0, 23.0));
        let inverse = moved.then(&scale).inverse().unwrap();
        assert_eq!(inverse.map(12.0, 23.0), (1.0, 1.0));
        assert!(Affine::placed(0.0, 0.0, [0.0; 4]).inverse().is_none());
        assert_eq!(
            transformed_rect(Rect::new(0, 0, 1, 1), 2, 3, ROT_90),
            Rect::new(2, 0, 3, 1)
        );
    }
}
