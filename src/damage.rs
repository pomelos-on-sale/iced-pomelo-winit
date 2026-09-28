//! The damage between two frames.
//!
//! iced computes damage by diffing what it drew: `iced_graphics::damage::diff` and
//! `Layer::damage` pair the previous frame's items with this frame's **by index**. That is sound
//! -- a changed item is always caught -- but it over-reports as soon as a frame adds or removes an
//! item in the middle of a list, because every item after it is compared against its neighbour.
//!
//! Measured on the launcher: one tile's pressed wash is one more quad, and the frame that shows it
//! damaged 82,027 px in 3 rectangles, against the tile's own 19,733. At the 4.5 µs/px this board
//! charges, that is the difference between a 60 ms frame and a 370 ms one. Nothing was wrong with
//! the pixels; the comparison was. It also explains the ~150 K px frames a drag across the grid
//! reported, which had been read as finger churn. Pairing the quads by geometry instead: **20,300 px
//! in 1 rectangle**, which is that tile and nothing else.
//!
//! Quads are the list this happens to, because they are painted in z-order into one layer: a
//! tile's wash is drawn *before* its icon, so it lands in the middle of the list. So the quads are
//! kept out of the layers and paired by *geometry* instead -- two quads on the same rectangle are
//! the same slot whatever was drawn between them -- and a merge-join over the two lists sorted by
//! bounds finds exactly what appeared, disappeared or changed. Sorting makes the pairing
//! independent of drawing order, which is what the index pairing was standing in for.
//!
//! Text, primitives and images stay iced's: their lists are appended to rather than inserted into,
//! so the index pairing is exact for them. A terminal *scrolling* its backlog off the top shifts
//! those lists and would still over-report; that is a recording problem, not a comparison one.

use std::cmp::Ordering;

use iced_core::renderer::Quad;
use iced_core::{Background, Rectangle};
use iced_tiny_skia::Layer;

/// The quads one layer drew, in the order it drew them.
type Quads = Vec<(Quad, Background)>;

/// What was drawn last frame, in the form the next frame's damage is computed against.
pub struct Scene {
    /// The layers with their quads taken out, so the rest of the diff can be iced's own.
    ///
    /// Emptying `quads` does not disturb `bounds`: the renderer computed those when it flushed,
    /// and nothing here recomputes them.
    layers: Vec<Layer>,
    /// Those quads, kept separately and paired by geometry.
    quads: Vec<Quads>,
}

impl Scene {
    pub fn new() -> Self {
        Self {
            layers: Vec::new(),
            quads: Vec::new(),
        }
    }

    /// The damage between this scene and `current`, and this scene replaced by it.
    pub fn advance(&mut self, current: &[Layer]) -> Vec<Rectangle> {
        let damage = self.damage(current);

        // Two clones of the recorded scene per frame: one to keep, one to hand `Layer::damage`
        // without its quads. The lists are tens of items, and a frame is tens of milliseconds.
        self.layers = current.to_vec();
        self.quads = current.iter().map(|layer| layer.quads.clone()).collect();

        for layer in &mut self.layers {
            layer.quads.clear();
        }

        damage
    }

    fn damage(&self, current: &[Layer]) -> Vec<Rectangle> {
        // A clip group appeared or vanished -- a widget that clips, such as a scrolling view, was
        // mounted or unmounted. The pairing below cannot be trusted across that, and it is rare
        // enough that repainting everything is the cheap answer.
        if self.layers.len() != current.len() {
            return current
                .iter()
                .map(|layer| layer.bounds)
                .chain(self.layers.iter().map(|layer| layer.bounds))
                .reduce(|total, bounds| total.union(&bounds))
                .into_iter()
                .collect();
        }

        let mut damage = Vec::new();

        for (index, (previous, now)) in self.layers.iter().zip(current).enumerate() {
            let Some(previous_quads) = self.quads.get(index) else {
                continue;
            };

            // iced's own diff for the rest of the layer, on layers whose quads are empty on both
            // sides: what it reports is then text, vector geometry and images, which is all that is
            // left of the layer.
            let mut rest = now.clone();
            rest.quads.clear();

            damage.extend(Layer::damage(previous, &rest));

            damage.extend(quads(previous_quads, &now.quads, now.bounds));
        }

        damage
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

/// The damage between two layers' quads.
///
/// Both lists are sorted by bounds first, which is what makes the pairing geometric: a merge-join
/// over two sorted lists visits the items in the same order whenever their geometry is the same,
/// so an insertion in the middle of one costs that insertion and nothing else.
fn quads(
    previous: &[(Quad, Background)],
    current: &[(Quad, Background)],
    clip: Rectangle,
) -> Vec<Rectangle> {
    let mut before: Vec<&(Quad, Background)> = previous.iter().collect();
    let mut after: Vec<&(Quad, Background)> = current.iter().collect();

    before.sort_by(|a, b| by_bounds(&a.0.bounds, &b.0.bounds));
    after.sort_by(|a, b| by_bounds(&a.0.bounds, &b.0.bounds));

    let mut damage = Vec::new();
    let (mut i, mut j) = (0, 0);

    loop {
        match (before.get(i), after.get(j)) {
            (None, None) => break,
            (Some(previous), None) => {
                damage.extend(clipped(&previous.0.bounds, clip));
                i += 1;
            }
            (None, Some(now)) => {
                damage.extend(clipped(&now.0.bounds, clip));
                j += 1;
            }
            (Some(previous), Some(now)) => match by_bounds(&previous.0.bounds, &now.0.bounds) {
                // The same rectangle in both frames: damage only if what it painted changed.
                Ordering::Equal => {
                    if *previous != *now {
                        damage.extend(clipped(&now.0.bounds, clip));
                    }

                    i += 1;
                    j += 1;
                }
                // Otherwise one of them has no counterpart, and its rectangle is damage.
                Ordering::Less => {
                    damage.extend(clipped(&previous.0.bounds, clip));
                    i += 1;
                }
                Ordering::Greater => {
                    damage.extend(clipped(&now.0.bounds, clip));
                    j += 1;
                }
            },
        }
    }

    damage
}

/// The rectangle a quad's damage occupies: grown by the pixel its antialiased edge bleeds into,
/// and clipped to the layer it was drawn in. The same rectangle iced reports for one quad.
pub(crate) fn clipped(bounds: &Rectangle, clip: Rectangle) -> Option<Rectangle> {
    bounds.expand(1.0).intersection(&clip)
}

/// The order that makes the pairing geometric, and that both scenes sort by.
pub(crate) fn by_bounds(a: &Rectangle, b: &Rectangle) -> Ordering {
    a.x.total_cmp(&b.x)
        .then_with(|| a.y.total_cmp(&b.y))
        .then_with(|| a.width.total_cmp(&b.width))
        .then_with(|| a.height.total_cmp(&b.height))
}
