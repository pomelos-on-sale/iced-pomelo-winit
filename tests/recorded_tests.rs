//! The recorded path, end to end: a real `Application`, our own renderer, and the panel.
//!
//! `surface_tests.rs` covers the other one, and the two share nothing but the panel buffer: that
//! path hands `tiny-skia` a pixmap of premultiplied RGBA and converts the damaged rectangles back,
//! and this one records commands and replays them straight into RGB565.
//!
//! The app draws **no text**, which is what makes that possible: text is the one part of the
//! renderer contract still unimplemented, so an app that draws a label would stop here.

#![cfg(feature = "renderer")]

use iced_core::theme::{Base, Mode};
use iced_core::{Background, Color, Element, Length, Point, Rectangle, Size, Theme};
use iced_widget::{container, Column};
use iced_winit::{App, Application, Renderer};

/// The panel these tests simulate.
const SIZE: u32 = 200;

/// The side of the square the app draws.
const SQUARE: f32 = 40.0;

fn screen() -> Rectangle {
    Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32))
}

/// Where the square is, depending on the app's one bit of state.
fn square_at(left: bool) -> Rectangle {
    let offset = if left { 0.0 } else { SQUARE };

    Rectangle::new(Point::new(offset, offset), Size::new(SQUARE, SQUARE))
}

/// The window background, which the surface paints and the tree never draws.
fn background() -> Color {
    <Theme as Base>::default(Mode::Dark).base().background_color
}

/// Draws a white square, in one corner of the screen or the other.
struct Square {
    left: bool,
}

impl App for Square {
    type Message = ();
    type Theme = Theme;

    fn theme(&self) -> Theme {
        <Theme as Base>::default(Mode::Dark)
    }

    fn update(&mut self, _message: ()) {}

    fn view(&self) -> Element<'_, (), Theme, Renderer> {
        let offset = if self.left { 0.0 } else { SQUARE };

        // The outer container is transparent and fills the screen, so what the tree draws is the
        // white square alone -- and the rest of the panel is the window background.
        container(
            container(Column::<(), Theme, Renderer>::new())
                .width(Length::Fixed(SQUARE))
                .height(Length::Fixed(SQUARE))
                .style(|_theme: &Theme| container::Style {
                    background: Some(Background::Color(Color::WHITE)),
                    ..container::Style::default()
                }),
        )
        .padding(offset)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

fn application() -> Application<Square> {
    Application::new(Square { left: true }, SIZE, SIZE).expect("the panel's buffers")
}

/// The RGB565 pixel at `(x, y)`.
fn at(app: &Application<Square>, x: u32, y: u32) -> u16 {
    app.panel().data()[(y * SIZE + x) as usize]
}

/// A colour as the panel stores it.
fn pixel(color: Color) -> u16 {
    let [r, g, b, _] = color.into_rgba8();

    pomelo_gfx::rgb888_to_rgb565(r, g, b)
}

/// The middle of a rectangle, which is inside it whatever its size.
fn middle(bounds: Rectangle) -> (u32, u32) {
    (
        (bounds.x + bounds.width / 2.0) as u32,
        (bounds.y + bounds.height / 2.0) as u32,
    )
}

#[test]
fn the_first_frame_draws_the_whole_screen() {
    let mut app = application();

    assert_eq!(
        app.frame(),
        vec![screen()],
        "nothing to diff against, so everything is drawn"
    );

    let (x, y) = middle(square_at(true));

    assert_eq!(at(&app, x, y), pixel(Color::WHITE), "the square is drawn");
    assert_eq!(
        at(&app, 150, 150),
        pixel(background()),
        "and the rest of the panel is the window background, which the tree never draws"
    );
}

#[test]
fn an_idle_screen_is_never_drawn() {
    let mut app = application();

    app.frame();

    assert!(
        app.frame().is_empty(),
        "an idle loop must not touch the panel"
    );
    assert!(app.frame().is_empty());
}

#[test]
fn a_square_that_moved_repaints_both_places() {
    let mut app = application();
    app.frame();

    app.update_app(|square| square.left = false);

    let damage = app.frame();
    let area: f32 = damage
        .iter()
        .map(|bounds| bounds.width * bounds.height)
        .sum();

    assert!(
        area < screen().width * screen().height,
        "a square that moved did not repaint the screen: {damage:?}"
    );

    // Where it went, and -- the assertion that matters -- where it was. Nothing in the recording
    // draws in the vacated place any more, so this passing means the surface cleared the damage
    // back to the window background before replaying over it.
    let (x, y) = middle(square_at(false));
    assert_eq!(
        at(&app, x, y),
        pixel(Color::WHITE),
        "the square is here now"
    );

    let (x, y) = middle(square_at(true));
    assert_eq!(
        at(&app, x, y),
        pixel(background()),
        "and it is gone from where it was"
    );
}
