//! The surface, driven the way the firmware will drive it: no window, no compositor, just
//! `Renderer::draw` into our buffer and a conversion into the panel's format.
//!
//! This is the `tiny-skia` presentation path, which only exists when the `renderer` feature is
//! off. The recorded path shares nothing with it but the panel buffer, so it has its own tests in
//! `recorded_tests.rs`.
#![cfg(not(feature = "renderer"))]

use iced_core::renderer::Quad;
// `reset` and `fill_quad` are methods of this trait, not of the concrete renderer.
use iced_core::renderer::Renderer as _;
use iced_core::{Background, Border, Color, Font, Pixels, Point, Rectangle, Shadow, Size};
use iced_tiny_skia::Renderer;
use iced_winit::Surface;
use pomelo_gfx::rgb888_to_rgb565;

/// Small enough to keep the tests quick; the firmware's panel is 480x480.
const SIZE: u32 = 64;

const RED: Color = Color::from_rgb(1.0, 0.0, 0.0);
const WHITE: Color = Color::from_rgb(1.0, 1.0, 1.0);

fn screen() -> Rectangle {
    Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32))
}

fn quad(x: f32, y: f32, side: f32) -> Quad {
    Quad {
        bounds: Rectangle::new(Point::new(x, y), Size::new(side, side)),
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

fn renderer() -> Renderer {
    Renderer::new(Font::default(), Pixels(16.0))
}

/// One pixel of the panel buffer, as a `u16`.
fn pixel(surface: &Surface, x: u32, y: u32) -> u16 {
    surface.panel().data()[(y * SIZE + x) as usize]
}

#[test]
fn the_first_frame_damages_everything_and_converts_it() {
    let mut renderer = renderer();
    let mut surface = Surface::new(SIZE, SIZE).expect("buffers");

    renderer.reset(screen());
    renderer.fill_quad(quad(8.0, 8.0, 16.0), Background::Color(RED));

    let damage = surface.present(&mut renderer, WHITE);

    // Nothing to diff against yet, so the whole screen is damaged.
    assert_eq!(damage, vec![screen()]);

    // The quad's centre is red, and outside it is the background.
    assert_eq!(pixel(&surface, 16, 16), rgb888_to_rgb565(255, 0, 0));
    assert_eq!(pixel(&surface, 36, 36), rgb888_to_rgb565(255, 255, 255));
    assert_eq!(pixel(&surface, 2, 2), rgb888_to_rgb565(255, 255, 255));
}

#[test]
fn an_unchanged_frame_damages_nothing() {
    let mut renderer = renderer();
    let mut surface = Surface::new(SIZE, SIZE).expect("buffers");

    renderer.reset(screen());
    renderer.fill_quad(quad(8.0, 8.0, 16.0), Background::Color(RED));
    assert!(!surface.present(&mut renderer, WHITE).is_empty());

    // Redraw the same frame: the layers are identical, so nothing is damaged and the panel
    // is not touched. This is the property that keeps an idle screen free.
    renderer.reset(screen());
    renderer.fill_quad(quad(8.0, 8.0, 16.0), Background::Color(RED));

    assert!(
        surface.present(&mut renderer, WHITE).is_empty(),
        "an identical frame must not damage anything"
    );
}

#[test]
fn the_union_of_the_damage_still_paints_all_of_it() {
    // Wider than `SIZE`: the two rectangles have to be far enough apart that `damage::group`
    // keeps them separate, which is the whole point of this test. At 64x64 it merges them.
    const WIDE: u32 = 256;

    let mut renderer = renderer();
    let mut surface = Surface::new(WIDE, WIDE).expect("buffers");

    let screen = Rectangle::with_size(Size::new(WIDE as f32, WIDE as f32));

    // Two quads in opposite corners.
    renderer.reset(screen);
    renderer.fill_quad(quad(4.0, 4.0, 8.0), Background::Color(RED));
    renderer.fill_quad(quad(240.0, 240.0, 8.0), Background::Color(RED));
    surface.present(&mut renderer, WHITE);

    renderer.reset(screen);
    renderer.fill_quad(quad(100.0, 100.0, 8.0), Background::Color(RED));
    renderer.fill_quad(quad(140.0, 140.0, 8.0), Background::Color(RED));

    let damage = surface.present(&mut renderer, WHITE);

    assert!(
        damage.len() > 1,
        "the two quads have to be a group, not one rectangle: {damage:?}"
    );

    // The renderer was handed one rectangle spanning both. Both quads have to be in the buffer
    // for that to be right, and so does what sits between them.
    let pixel = |x: u32, y: u32| surface.panel().data()[(y * WIDE + x) as usize];

    assert_eq!(pixel(104, 104), rgb888_to_rgb565(255, 0, 0));
    assert_eq!(pixel(144, 144), rgb888_to_rgb565(255, 0, 0));
    assert_eq!(pixel(124, 124), rgb888_to_rgb565(255, 255, 255));
}

#[test]
fn only_the_changed_rectangle_is_damaged() {
    let mut renderer = renderer();
    let mut surface = Surface::new(SIZE, SIZE).expect("buffers");

    renderer.reset(screen());
    renderer.fill_quad(quad(8.0, 8.0, 16.0), Background::Color(RED));
    surface.present(&mut renderer, WHITE);

    // Move the quad far enough away to be a separate region.
    renderer.reset(screen());
    renderer.fill_quad(quad(40.0, 40.0, 8.0), Background::Color(RED));

    let damage = surface.present(&mut renderer, WHITE);

    assert!(!damage.is_empty(), "the frame changed");
    assert!(
        damage != vec![screen()],
        "but only part of it should have been damaged, got {damage:?}"
    );

    // And the converted pixels agree: the new quad is there, its old position is not.
    assert_eq!(pixel(&surface, 44, 44), rgb888_to_rgb565(255, 0, 0));
    assert_eq!(pixel(&surface, 16, 16), rgb888_to_rgb565(255, 255, 255));
}
