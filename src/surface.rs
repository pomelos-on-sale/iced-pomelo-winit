//! The `tiny-skia` path's frame buffers, and the step that turns a frame into panel pixels.
//!
//! `iced_tiny_skia` is a *recorder*: `Renderer::draw` takes the pixel buffer from the caller,
//! along with the rectangles that changed. Everything on either side of that call is ours.
//!
//! This module exists for the path where the rasteriser *is* `tiny-skia`. With the `renderer`
//! feature the frame is drawn by `pomelo-gfx` instead, and the surface — the panel buffer and the
//! recorded damage — lives in that crate, next to the commands it replays: see
//! `iced_pomelo_gfx::Surface`, which this crate re-exports under the same name.

use iced_core::{Color, Rectangle, Size};
use iced_graphics::Viewport;
use iced_tiny_skia::Renderer;
use pomelo_gfx::rgb888_to_rgb565;
use pomelo_gfx::Pixmap565;

use crate::damage::Scene;

/// Everything a frame passes through.
///
/// Three buffers, each with a reason to exist:
///
/// * `rgba` — what the renderer draws into: `width * height * 4` bytes of premultiplied 8888,
///   with no row padding to account for. `PixmapMut::from_bytes` would like R, G, B, A, but
///   `iced_tiny_skia` writes **B, G, R, A** — see `convert`.
/// * `mask` — iced's clip mask, one byte per pixel.
/// * `panel` — the RGB565 frame buffer the panel is written from.
///
/// It also remembers the previous frame's layers, because that is how iced computes damage:
/// by diffing what it drew last time against what it drew this time, rather than by asking
/// widgets to report their own invalidations.
pub struct Surface {
    rgba: Vec<u8>,
    mask: tiny_skia::Mask,
    panel: Pixmap565,
    scene: Scene,
    background: Color,
    viewport: Viewport,
}

impl Surface {
    /// Allocates the buffers for a panel of `width * height` physical pixels.
    ///
    /// `None` if the size is zero or the allocation fails — the panel is 480×480, so the
    /// RGBA surface is 900 KiB and must land in PSRAM rather than internal SRAM.
    pub fn new(width: u32, height: u32) -> Option<Self> {
        Some(Self {
            rgba: vec![0; (width as usize) * (height as usize) * 4],
            mask: tiny_skia::Mask::new(width, height)?,
            panel: Pixmap565::new(width, height)?,
            scene: Scene::new(),
            // Deliberately not black: `present` treats a background change as "everything may
            // have moved", so this makes the first frame report full damage. iced's own
            // compositor gets the same effect by starting from `Color::TRANSPARENT`.
            background: Color::TRANSPARENT,
            // The panel is 1:1 — no HiDPI scaling anywhere in this stack.
            viewport: Viewport::with_physical_size(Size::new(width, height), 1.0),
        })
    }

    /// The viewport this surface draws for.
    pub fn viewport(&self) -> Viewport {
        self.viewport.clone()
    }

    /// The RGB565 frame buffer, ready to be handed to the panel.
    pub fn panel(&self) -> &Pixmap565 {
        &self.panel
    }

    /// Replays the renderer's damage into the panel buffer and returns the rectangles that
    /// changed, in physical pixels.
    ///
    /// An empty result means nothing moved and the panel does not need to be touched at all.
    pub fn present(&mut self, renderer: &mut Renderer, background: Color) -> Vec<Rectangle> {
        let screen = Rectangle::with_size(self.viewport.logical_size());

        // The scene is recorded whether or not this frame's diff is used: the next frame compares
        // against it.
        let changed = self.scene.advance(renderer.layers());

        // A changed background invalidates every pixel, so there is nothing useful to diff.
        let damage = if self.background == background {
            changed
        } else {
            vec![screen]
        };

        self.background = background;

        if damage.is_empty() {
            return Vec::new();
        }

        let damage = iced_graphics::damage::group(damage, screen);

        // One rectangle is handed to the renderer, not the group.
        //
        // `tiny-skia` rasterises a primitive over its full bounds and uses the clip mask only to
        // reject pixels at blend time, so *every* damage rectangle costs a full pass over the
        // layer's primitives. Measured on this board: 4.5 µs/px, about a second for a full-screen
        // pass — and a drag across the launcher's grid damages six tiles, so six seconds, which is
        // enough to trip the 5 s task watchdog. The union is one pass for the same result, and the
        // pixels it repaints that did not change are free next to five more passes. This is a
        // `tiny-skia` property and not an iced one: when `pomelo-gfx` rasterises into the damage
        // directly, this goes back to the group.
        let total = damage
            .iter()
            .fold(damage[0], |total, bounds| total.union(bounds));

        {
            let mut pixels = tiny_skia::PixmapMut::from_bytes(
                &mut self.rgba,
                self.viewport.physical_width(),
                self.viewport.physical_height(),
            )
            .expect("the surface holds exactly this many pixels");

            renderer.draw(
                &mut pixels,
                &mut self.mask,
                &self.viewport,
                &[total],
                background,
            );
        }

        // Everything the renderer drew is inside these rectangles and nowhere else, so only
        // they need converting.
        let damaged: Vec<Rectangle> = damage
            .iter()
            .map(|bounds| *bounds * self.viewport.scale_factor())
            .collect();

        for bounds in &damaged {
            self.convert(*bounds);
        }

        damaged
    }

    /// Copies one rectangle from the 8888 surface into the RGB565 frame buffer.
    ///
    /// The surface is premultiplied, and `Renderer::draw` has just cleared this rectangle to
    /// the background and drawn over it, so the stored RGB *is* the composited colour:
    /// taking it straight and dropping alpha is exactly what an opaque RGB565 buffer wants.
    ///
    /// The one subtlety is the byte order, and it is measured rather than assumed.
    /// `PixmapMut::from_bytes` documents R, G, B, A, and `tiny_skia` does write that order —
    /// but `iced_tiny_skia` swaps red and blue before the rasterizer ever sees a colour,
    /// because it renders for softbuffer's `0x00RRGGBB` buffer: `engine::into_color` is
    /// `from_rgba(color.b, color.g, color.r, color.a)`, `geometry::into_paint` matches it, and
    /// glyph packing (`text.rs`) and image decoding (`raster.rs`) do the same. A red quad
    /// arrives here as `0, 0, 255, 255`; reading byte 0 as red packs it as RGB565 blue.
    ///
    /// The swap is applied on the way in at every single entry point and nowhere else, and
    /// blending is per-channel, so inverting it once here is exact — not a colour correction.
    ///
    /// No dithering. A smooth 8-bit gradient quantised to 565 will band, and `pomelo-gfx` has a
    /// Bayer matrix for precisely this — but it is private to its rasterizer, and exposing it
    /// is a change in a different repository now. Deliberately deferred rather than forgotten.
    fn convert(&mut self, bounds: Rectangle) {
        let (width, height) = (
            self.viewport.physical_width(),
            self.viewport.physical_height(),
        );

        let x0 = bounds.x.max(0.0) as u32;
        let y0 = bounds.y.max(0.0) as u32;
        let x1 = ((bounds.x + bounds.width).max(0.0) as u32).min(width);
        let y1 = ((bounds.y + bounds.height).max(0.0) as u32).min(height);

        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let rgba = &self.rgba;
        let panel = self.panel.data_mut();
        let run = (x1 - x0) as usize;

        for y in y0..y1 {
            let row = (y * width) as usize;
            let first = row * 4 + x0 as usize * 4;

            let source = &rgba[first..first + run * 4];
            let target = &mut panel[row + x0 as usize..row + x0 as usize + run];

            for (x, pixel) in target.iter_mut().enumerate() {
                let src = x * 4;

                // Byte 0 is blue and byte 2 is red — iced writes `b, g, r, a`. Byte 3 is
                // alpha, which RGB565 has no room for.
                *pixel = rgb888_to_rgb565(source[src + 2], source[src + 1], source[src]);
            }
        }
    }
}
