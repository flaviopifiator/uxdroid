//! The layer tree below `SurfaceControl`, the part SurfaceFlinger keeps on Android: each layer's
//! geometry, which only transactions change, the order layers draw in, and what changed in a window
//! since it was last composed.
//!
//! A transaction collects changes and applies them all at once, under one process-wide lock that
//! composition also takes to read the tree, so a window never shows half of a transaction.
//!
//! Order follows SurfaceFlinger: a layer draws its children with a negative z below its own frame
//! and the others above, by z, and a layer placed relative to another draws among that layer's
//! children while keeping its own parent's position, clip, alpha and visibility. A layer's clip is
//! its crop and its frame's size, and its children are clipped to it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::compose::{self, Affine, Item, Rect, Round};

/// Held while a transaction applies and while composition reads the tree.
static SCENE: Mutex<()> = Mutex::new(());

fn scene() -> MutexGuard<'static, ()> {
    SCENE.lock().unwrap_or_else(|p| p.into_inner())
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// `SurfaceControl`'s `HIDDEN` flag, of `setFlags`.
pub const HIDDEN: u32 = 0x1;

struct Geometry<X> {
    parent: Option<Arc<Layer<X>>>,
    /// Set by `setRelativeLayer`; cleared by `setLayer`.
    relative: Option<Weak<Layer<X>>>,
    x: f32,
    y: f32,
    z: i32,
    hidden: bool,
    alpha: f32,
    matrix: [f32; 4],
    crop: Option<[f32; 4]>,
    buffer_transform: u32,
    corner_radius: f32,
    /// Detached from its window: a layer reparented to nothing is not drawn anywhere.
    offscreen: bool,
}

/// A layer's last frame, 0xAARRGGBB, and the part of it posted since its window last composed.
#[derive(Default)]
pub struct Content {
    pub pixels: Vec<u32>,
    pub width: i32,
    pub height: i32,
    pub damage: Rect,
}

/// One layer. `X` is what the platform keeps beside it, such as its Wayland window.
pub struct Layer<X> {
    pub name: String,
    /// A window's own layer, which no parent holds: its frames are shown in their own window.
    window: bool,
    geometry: Mutex<Geometry<X>>,
    children: Mutex<Vec<Weak<Layer<X>>>>,
    /// Layers placed relative to this one.
    relatives: Mutex<Vec<Weak<Layer<X>>>>,
    content: Mutex<Content>,
    pub extra: X,
}

impl<X> Layer<X> {
    /// A layer under `parent`, or a window's own layer when there is none.
    pub fn new(name: String, parent: Option<Arc<Layer<X>>>, hidden: bool, extra: X) -> Arc<Self> {
        let window = parent.is_none();
        let layer = Arc::new(Layer {
            name,
            window,
            geometry: Mutex::new(Geometry {
                parent: None,
                relative: None,
                x: 0.0,
                y: 0.0,
                z: 0,
                hidden,
                alpha: 1.0,
                matrix: [1.0, 0.0, 0.0, 1.0],
                crop: None,
                buffer_transform: 0,
                corner_radius: 0.0,
                offscreen: false,
            }),
            children: Mutex::new(Vec::new()),
            relatives: Mutex::new(Vec::new()),
            content: Mutex::new(Content::default()),
            extra,
        });
        if parent.is_some() {
            let _scene = scene();
            layer.set_parent(parent);
        }
        layer
    }

    /// The last frame. Lock it alone, or after the tree: never hold it while locking another.
    pub fn content(&self) -> MutexGuard<'_, Content> {
        locked(&self.content)
    }

    pub fn parent(&self) -> Option<Arc<Layer<X>>> {
        locked(&self.geometry).parent.clone()
    }

    /// The layer whose window shows this one, if any: following parents up to a window's own
    /// layer. A layer whose chain ends anywhere else is offscreen.
    pub fn window_layer(self: &Arc<Self>) -> Option<Arc<Layer<X>>> {
        let mut layer = self.clone();
        loop {
            let (parent, offscreen) = {
                let geometry = locked(&layer.geometry);
                (geometry.parent.clone(), geometry.offscreen)
            };
            match parent {
                Some(parent) => layer = parent,
                None if layer.window && !offscreen => return Some(layer),
                None => return None,
            }
        }
    }

    /// Moves this layer under `parent`, refusing a parent that is the layer or one of its
    /// descendants. Called with the scene locked.
    fn set_parent(self: &Arc<Self>, parent: Option<Arc<Layer<X>>>) {
        let mut ancestor = parent.clone();
        while let Some(layer) = ancestor {
            if Arc::ptr_eq(&layer, self) {
                return;
            }
            ancestor = layer.parent();
        }
        if let Some(parent) = &parent {
            let mut children = locked(&parent.children);
            children.retain(|child| child.strong_count() > 0);
            children.push(Arc::downgrade(self));
        }
        let detached = parent.is_none();
        let old = {
            let mut geometry = locked(&self.geometry);
            geometry.offscreen = detached;
            std::mem::replace(&mut geometry.parent, parent)
        };
        if let Some(old) = old {
            locked(&old.children).retain(|child| !std::ptr::eq(child.as_ptr(), Arc::as_ptr(self)));
        }
    }

    fn set_relative(self: &Arc<Self>, relative: Option<&Arc<Layer<X>>>) {
        let old = {
            let mut geometry = locked(&self.geometry);
            std::mem::replace(&mut geometry.relative, relative.map(Arc::downgrade))
        };
        if let Some(old) = old.and_then(|old| old.upgrade()) {
            locked(&old.relatives).retain(|r| !std::ptr::eq(r.as_ptr(), Arc::as_ptr(self)));
        }
        if let Some(relative) = relative {
            let mut relatives = locked(&relative.relatives);
            relatives.retain(|r| r.strong_count() > 0);
            relatives.push(Arc::downgrade(self));
        }
    }

    /// Called with the scene locked.
    fn change(self: &Arc<Self>, change: Change<X>) {
        match change {
            Change::Reparent(parent) => self.set_parent(parent),
            Change::RelativeLayer(relative, z) => {
                if !Arc::ptr_eq(&relative, self) {
                    self.set_relative(Some(&relative));
                    locked(&self.geometry).z = z;
                }
            }
            Change::Layer(z) => {
                self.set_relative(None);
                locked(&self.geometry).z = z;
            }
            change => {
                let mut geometry = locked(&self.geometry);
                match change {
                    Change::Position(x, y) => (geometry.x, geometry.y) = (x, y),
                    Change::Alpha(alpha) => geometry.alpha = alpha.clamp(0.0, 1.0),
                    Change::Matrix(matrix) => geometry.matrix = matrix,
                    Change::Crop(crop) => {
                        // An empty or invalid crop is no crop, as in SurfaceFlinger.
                        geometry.crop = crop.filter(|[l, t, r, b]| r > l && b > t);
                    }
                    Change::CornerRadius(radius) => geometry.corner_radius = radius.max(0.0),
                    Change::BufferTransform(transform) => geometry.buffer_transform = transform & 7,
                    Change::Flags { flags, mask } => {
                        if mask & HIDDEN != 0 {
                            geometry.hidden = flags & HIDDEN != 0;
                        }
                    }
                    Change::Reparent(_) | Change::RelativeLayer(..) | Change::Layer(_) => {}
                }
            }
        }
    }
}

/// One change of a layer, as a `SurfaceControl.Transaction` setter makes it.
pub enum Change<X> {
    Position(f32, f32),
    /// z among the parent's children; also undoes `RelativeLayer`.
    Layer(i32),
    /// z among the children of another layer.
    RelativeLayer(Arc<Layer<X>>, i32),
    Reparent(Option<Arc<Layer<X>>>),
    Alpha(f32),
    /// dsdx, dtdx, dtdy, dsdy.
    Matrix([f32; 4]),
    /// left, top, right, bottom in the layer's space.
    Crop(Option<[f32; 4]>),
    CornerRadius(f32),
    BufferTransform(u32),
    Flags {
        flags: u32,
        mask: u32,
    },
}

/// Changes waiting to be applied together, in the order they were made.
pub struct Transaction<X> {
    changes: Vec<(Arc<Layer<X>>, Change<X>)>,
}

impl<X> Default for Transaction<X> {
    fn default() -> Self {
        Transaction {
            changes: Vec::new(),
        }
    }
}

impl<X> Transaction<X> {
    pub fn set(&mut self, layer: Arc<Layer<X>>, change: Change<X>) {
        self.changes.push((layer, change));
    }

    /// Takes every change of `other` after this one's, leaving `other` empty, as Android's
    /// `Transaction.merge`.
    pub fn merge(&mut self, other: &mut Transaction<X>) {
        self.changes.append(&mut other.changes);
    }

    pub fn clear(&mut self) {
        self.changes.clear();
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Applies every change at once and empties the transaction. Returns the window layers whose
    /// windows may show something else now, before and after the changes.
    pub fn apply(&mut self) -> Vec<Arc<Layer<X>>> {
        let changes = std::mem::take(&mut self.changes);
        if changes.is_empty() {
            return Vec::new();
        }
        let mut windows: Vec<Arc<Layer<X>>> = Vec::new();
        let mut note = |layer: Option<Arc<Layer<X>>>| {
            if let Some(layer) = layer
                && !windows.iter().any(|known| Arc::ptr_eq(known, &layer))
            {
                windows.push(layer);
            }
        };
        let layers: Vec<Arc<Layer<X>>> = changes.iter().map(|(layer, _)| layer.clone()).collect();
        let _scene = scene();
        for layer in &layers {
            note(layer.window_layer());
        }
        for (layer, change) in changes {
            layer.change(change);
        }
        for layer in &layers {
            note(layer.window_layer());
        }
        windows
    }
}

/// Where one layer draws, read from the tree under the scene lock.
#[derive(Clone, Copy, PartialEq)]
struct Placement {
    transform: Affine,
    clip: Rect,
    alpha: f32,
    buffer_transform: u32,
    round: Option<Round>,
}

/// What one window showed at its last composition, to find what changed since.
#[derive(Default)]
pub struct Composer {
    shown: HashMap<usize, (Placement, Rect, usize)>,
    size: (i32, i32),
}

/// The geometry each visible layer gets from its parents.
struct Walk<X> {
    placements: HashMap<usize, (Arc<Layer<X>>, Placement)>,
}

impl<X> Walk<X> {
    fn visit(&mut self, layer: &Arc<Layer<X>>, parent: &Placement) {
        let (placement, children) = {
            let geometry = locked(&layer.geometry);
            if geometry.hidden || geometry.alpha <= 0.0 {
                return;
            }
            let transform =
                parent
                    .transform
                    .then(&Affine::placed(geometry.x, geometry.y, geometry.matrix));
            let content = locked(&layer.content);
            let (width, height) =
                compose::transformed_size(content.width, content.height, geometry.buffer_transform);
            drop(content);
            // The layer's own bounds: its crop, within its frame when it has one.
            let mut bounds: Option<[f32; 4]> = geometry.crop;
            if width > 0 && height > 0 {
                let frame = [0.0, 0.0, width as f32, height as f32];
                bounds = Some(match bounds {
                    Some([l, t, r, b]) => {
                        [l.max(0.0), t.max(0.0), r.min(frame[2]), b.min(frame[3])]
                    }
                    None => frame,
                });
            }
            let clip = match bounds {
                Some(bounds) => parent.clip.intersect(&transform.bounds(bounds)),
                None => parent.clip,
            };
            let round = match (geometry.corner_radius > 0.0, bounds) {
                (true, Some([l, t, r, b])) if transform.keeps_axes() => {
                    let (x0, y0) = transform.map(l, t);
                    let (x1, y1) = transform.map(r, b);
                    let scale = (transform.a.abs() + transform.b.abs())
                        .min(transform.c.abs() + transform.d.abs());
                    Some(Round {
                        rect: [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)],
                        radius: geometry.corner_radius * scale,
                    })
                }
                _ => parent.round,
            };
            let placement = Placement {
                transform,
                clip,
                alpha: parent.alpha * geometry.alpha,
                buffer_transform: geometry.buffer_transform,
                round,
            };
            (placement, live(&layer.children))
        };
        self.placements
            .insert(Arc::as_ptr(layer) as usize, (layer.clone(), placement));
        for child in children {
            self.visit(&child, &placement);
        }
    }

    /// Appends `layer` and the layers drawn with it to `order`, bottom first.
    fn order(&self, layer: &Arc<Layer<X>>, seen: &mut HashSet<usize>, order: &mut Vec<usize>) {
        let key = Arc::as_ptr(layer) as usize;
        if !self.placements.contains_key(&key) || !seen.insert(key) {
            return;
        }
        let mut stack: Vec<(i32, Arc<Layer<X>>)> = live(&layer.children)
            .into_iter()
            .chain(live(&layer.relatives))
            .filter_map(|child| {
                let geometry = locked(&child.geometry);
                let relative_here = match &geometry.relative {
                    Some(relative) => std::ptr::eq(relative.as_ptr(), Arc::as_ptr(layer)),
                    None => geometry
                        .parent
                        .as_ref()
                        .is_some_and(|parent| Arc::ptr_eq(parent, layer)),
                };
                let z = geometry.z;
                drop(geometry);
                relative_here.then_some((z, child))
            })
            .collect();
        stack.sort_by_key(|(z, _)| *z);
        let split = stack.partition_point(|(z, _)| *z < 0);
        for (_, child) in &stack[..split] {
            self.order(child, seen, order);
        }
        order.push(key);
        for (_, child) in &stack[split..] {
            self.order(child, seen, order);
        }
    }
}

fn live<X>(layers: &Mutex<Vec<Weak<Layer<X>>>>) -> Vec<Arc<Layer<X>>> {
    locked(layers).iter().filter_map(Weak::upgrade).collect()
}

impl Composer {
    /// Redraws what changed in `window`'s frame, `width` by `height`, since the last call, and
    /// returns the damaged rectangle, empty when nothing changed. The first call, and the first
    /// after the size changes, redraws everything.
    pub fn compose<X>(
        &mut self,
        window: &Arc<Layer<X>>,
        frame: &mut [u32],
        width: i32,
        height: i32,
    ) -> Rect {
        let (walk, order) = {
            let _scene = scene();
            let mut walk = Walk {
                placements: HashMap::new(),
            };
            let top = Placement {
                transform: Affine::IDENTITY,
                clip: Rect::sized(width, height),
                alpha: 1.0,
                buffer_transform: 0,
                round: None,
            };
            walk.visit(window, &top);
            let mut order = Vec::new();
            walk.order(window, &mut HashSet::new(), &mut order);
            (walk, order)
        };
        let contents: Vec<(MutexGuard<'_, Content>, Placement, usize)> = order
            .iter()
            .filter_map(|key| walk.placements.get(key).map(|entry| (*key, entry)))
            .map(|(key, (layer, placement))| (layer.content(), *placement, key))
            .collect();
        let mut damage = Rect::EMPTY;
        if self.size != (width, height) {
            damage = Rect::sized(width, height);
        }
        let mut shown = HashMap::with_capacity(contents.len());
        let mut items = Vec::with_capacity(contents.len());
        for (index, (content, placement, key)) in contents.iter().enumerate() {
            let item = Item {
                pixels: &content.pixels,
                width: content.width,
                height: content.height,
                transform: placement.transform,
                buffer_transform: placement.buffer_transform,
                clip: placement.clip,
                alpha: placement.alpha,
                round: placement.round,
            };
            let screen = item.screen();
            match self.shown.remove(key) {
                Some((before, area, place))
                    if before == *placement && area == screen && place == index =>
                {
                    let posted = compose::transformed_rect(
                        content.damage,
                        content.width,
                        content.height,
                        placement.buffer_transform,
                    );
                    if !posted.is_empty() {
                        let posted = placement.transform.bounds([
                            posted.left as f32,
                            posted.top as f32,
                            posted.right as f32,
                            posted.bottom as f32,
                        ]);
                        damage = damage.union(&posted.intersect(&screen));
                    }
                }
                Some((_, area, _)) => damage = damage.union(&area).union(&screen),
                None => damage = damage.union(&screen),
            }
            shown.insert(*key, (*placement, screen, index));
            items.push(item);
        }
        // Layers that are no longer drawn leave their area to redraw.
        for (_, area, _) in self.shown.values() {
            damage = damage.union(area);
        }
        self.shown = shown;
        self.size = (width, height);
        let damage = damage.intersect(&Rect::sized(width, height));
        compose::compose(frame, width, height, damage, &items);
        drop(items);
        for (mut content, _, _) in contents {
            content.damage = Rect::EMPTY;
        }
        damage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: u32 = 0xffff_0000;
    const BLUE: u32 = 0xff00_00ff;
    const GREEN: u32 = 0xff00_ff00;
    const CLEAR: u32 = 0;
    const BLACK: u32 = 0xff00_0000;

    type Test = Arc<Layer<()>>;

    fn window(width: i32, height: i32, color: u32) -> Test {
        let layer = Layer::new("window".into(), None, false, ());
        post(&layer, width, height, color);
        layer
    }

    fn child(parent: &Test, width: i32, height: i32, color: u32) -> Test {
        let layer = Layer::new("child".into(), Some(parent.clone()), false, ());
        post(&layer, width, height, color);
        layer
    }

    fn post(layer: &Test, width: i32, height: i32, color: u32) {
        let mut content = layer.content();
        content.pixels = vec![color; (width * height) as usize];
        (content.width, content.height) = (width, height);
        content.damage = Rect::sized(width, height);
    }

    fn compose(
        composer: &mut Composer,
        window: &Test,
        width: i32,
        height: i32,
    ) -> (Vec<u32>, Rect) {
        let mut frame = vec![GREEN; (width * height) as usize];
        let damage = composer.compose(window, &mut frame, width, height);
        (frame, damage)
    }

    fn one(window: &Test, width: i32, height: i32) -> Vec<u32> {
        compose(&mut Composer::default(), window, width, height).0
    }

    fn apply(changes: Vec<(&Test, Change<()>)>) -> Vec<Test> {
        let mut transaction = Transaction::default();
        for (layer, change) in changes {
            transaction.set(layer.clone(), change);
        }
        transaction.apply()
    }

    #[test]
    fn changes_wait_for_apply_and_then_apply_together() {
        let root = window(3, 1, CLEAR);
        let layer = child(&root, 1, 1, RED);
        let mut transaction = Transaction::default();
        transaction.set(layer.clone(), Change::Position(2.0, 0.0));
        transaction.set(layer.clone(), Change::Alpha(0.0));

        assert_eq!(
            one(&root, 3, 1),
            [RED, BLACK, BLACK],
            "applied before apply"
        );
        let windows = transaction.apply();

        assert!(transaction.is_empty());
        assert_eq!(windows.len(), 1);
        assert!(Arc::ptr_eq(&windows[0], &root));
        // Both changes at once: moved and invisible, never moved and visible.
        assert_eq!(one(&root, 3, 1), [BLACK, BLACK, BLACK]);
    }

    #[test]
    fn merge_takes_the_other_changes_in_order_and_clear_drops_them() {
        let root = window(3, 1, CLEAR);
        let layer = child(&root, 1, 1, RED);
        let mut first = Transaction::default();
        let mut second = Transaction::default();
        first.set(layer.clone(), Change::Position(1.0, 0.0));
        second.set(layer.clone(), Change::Position(2.0, 0.0));

        first.merge(&mut second);
        assert!(second.is_empty());
        second.apply();
        assert_eq!(
            one(&root, 3, 1),
            [RED, BLACK, BLACK],
            "the merged-from transaction kept changes"
        );
        first.apply();
        // The later change wins.
        assert_eq!(one(&root, 3, 1), [BLACK, BLACK, RED]);

        let mut dropped = Transaction::default();
        dropped.set(layer.clone(), Change::Position(0.0, 0.0));
        dropped.clear();
        dropped.apply();
        assert_eq!(one(&root, 3, 1), [BLACK, BLACK, RED]);
    }

    #[test]
    fn negative_z_draws_below_the_parent_and_siblings_by_z() {
        // The window's left pixel is a hole.
        let root = window(2, 1, RED);
        root.content().pixels[0] = CLEAR;
        let below = child(&root, 2, 1, BLUE);
        let first = child(&root, 1, 1, GREEN);
        let second = child(&root, 1, 1, BLUE);
        apply(vec![
            (&below, Change::Layer(-1)),
            (&first, Change::Layer(2)),
            (&first, Change::Position(1.0, 0.0)),
            (&second, Change::Layer(1)),
            (&second, Change::Position(1.0, 0.0)),
        ]);

        // Blue below shows through the hole; at 1, z 2 (green) covers z 1 (blue).
        assert_eq!(one(&root, 2, 1), [BLUE, GREEN]);
    }

    #[test]
    fn a_relative_layer_draws_among_the_other_layers_children_with_its_own_parent_geometry() {
        // As SurfaceView: its layer is a child of a bounds layer and relative to the window, below.
        let root = window(3, 1, RED);
        root.content().pixels[1] = CLEAR;
        let bounds = Layer::new("bounds".into(), Some(root.clone()), false, ());
        apply(vec![(&bounds, Change::Position(1.0, 0.0))]);
        let surface = child(&bounds, 1, 1, BLUE);
        apply(vec![(&surface, Change::RelativeLayer(root.clone(), -2))]);

        // Drawn below the window, at its parent's position: it shows through the hole only.
        assert_eq!(one(&root, 3, 1), [RED, BLUE, RED]);

        apply(vec![(&surface, Change::Layer(0))]);
        // Back among its own parent's children, above the window.
        assert_eq!(one(&root, 3, 1), [RED, BLUE, RED]);
        apply(vec![(&surface, Change::Position(1.0, 0.0))]);
        assert_eq!(one(&root, 3, 1), [RED, BLACK, BLUE]);
    }

    #[test]
    fn hidden_layers_hide_their_children_and_alpha_multiplies() {
        let root = window(2, 1, CLEAR);
        let parent = child(&root, 2, 1, CLEAR);
        let layer = child(&parent, 1, 1, 0xffff_ffff);
        apply(vec![(
            &parent,
            Change::Flags {
                flags: HIDDEN,
                mask: HIDDEN,
            },
        )]);
        assert_eq!(one(&root, 2, 1), [BLACK, BLACK]);

        apply(vec![
            (
                &parent,
                Change::Flags {
                    flags: 0,
                    mask: HIDDEN,
                },
            ),
            (&parent, Change::Alpha(0.5)),
            (&layer, Change::Alpha(0.5)),
        ]);
        // A quarter of white over black.
        assert_eq!(one(&root, 2, 1), [0xff40_4040, BLACK]);
    }

    #[test]
    fn crops_clip_the_layer_and_its_children_and_transforms_compose() {
        let root = window(4, 1, CLEAR);
        let parent = Layer::new("container".into(), Some(root.clone()), false, ());
        let layer = child(&parent, 1, 1, RED);
        apply(vec![
            (&parent, Change::Position(1.0, 0.0)),
            (&parent, Change::Crop(Some([0.0, 0.0, 2.0, 1.0]))),
            (&layer, Change::Matrix([4.0, 0.0, 0.0, 1.0])),
        ]);

        // Four pixels wide from x 1, clipped by the parent's crop to two.
        assert_eq!(one(&root, 4, 1), [BLACK, RED, RED, BLACK]);
    }

    #[test]
    fn corner_radius_and_buffer_transform_reach_the_window() {
        let root = window(4, 4, CLEAR);
        let layer = child(&root, 4, 4, RED);
        apply(vec![(&layer, Change::CornerRadius(2.0))]);
        let frame = one(&root, 4, 4);
        assert_eq!(frame[0], BLACK);
        assert_eq!(frame[5], RED);

        let turned = window(2, 1, RED);
        turned.content().pixels[1] = BLUE;
        apply(vec![(&turned, Change::BufferTransform(compose::FLIP_H))]);
        assert_eq!(one(&turned, 2, 1), [BLUE, RED]);
    }

    #[test]
    fn only_what_changed_is_damaged() {
        let root = window(4, 4, RED);
        let layer = child(&root, 1, 1, BLUE);
        apply(vec![(&layer, Change::Position(1.0, 1.0))]);
        let mut composer = Composer::default();

        let (_, damage) = compose(&mut composer, &root, 4, 4);
        assert_eq!(damage, Rect::sized(4, 4), "the first composition draws all");
        let (frame, damage) = compose(&mut composer, &root, 4, 4);
        assert_eq!(damage, Rect::EMPTY, "nothing changed");
        assert!(frame.iter().all(|&pixel| pixel == GREEN), "nothing redrawn");

        // A post damages its own pixels only, where the window shows them.
        post(&layer, 1, 1, GREEN);
        let (frame, damage) = compose(&mut composer, &root, 4, 4);
        assert_eq!(damage, Rect::new(1, 1, 2, 2));
        assert_eq!(frame[5], GREEN);

        // A move damages where the layer was and where it is.
        apply(vec![(&layer, Change::Position(3.0, 3.0))]);
        let (_, damage) = compose(&mut composer, &root, 4, 4);
        assert_eq!(damage, Rect::new(1, 1, 4, 4));

        // The window's own partial post.
        root.content().damage = Rect::new(0, 0, 1, 1);
        let (_, damage) = compose(&mut composer, &root, 4, 4);
        assert_eq!(damage, Rect::new(0, 0, 1, 1));
    }

    #[test]
    fn a_layer_reparented_to_nothing_leaves_its_window() {
        let root = window(2, 1, CLEAR);
        let layer = child(&root, 1, 1, RED);
        let mut composer = Composer::default();
        compose(&mut composer, &root, 2, 1);

        let windows = apply(vec![(&layer, Change::Reparent(None))]);

        assert_eq!(windows.len(), 1, "the old window must redraw");
        assert!(
            layer.window_layer().is_none(),
            "offscreen, not a window of its own"
        );
        let (frame, damage) = compose(&mut composer, &root, 2, 1);
        assert_eq!(damage, Rect::new(0, 0, 1, 1));
        assert_eq!(frame, [BLACK, GREEN]);
        assert!(root.window_layer().is_some_and(|w| Arc::ptr_eq(&w, &root)));
    }
}
