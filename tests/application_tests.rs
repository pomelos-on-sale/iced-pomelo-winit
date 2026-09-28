//! The loop, driven the way the firmware will drive it: touches in, damaged rectangles out.
//!
//! No window, no compositor, no async runtime — the widget tree, the events, the renderer and
//! the panel buffer are the whole system.
//!
//! The app here draws a **label**, so this runs on both renderers: the `tiny-skia` path by
//! default, and the recorded one with the `renderer` feature.

use std::cell::Cell;
use std::rc::Rc;

use iced_core::theme::{Base, Mode};
use iced_core::{Element, Length, Point, Rectangle, Size, Theme};
use iced_widget::{button, container, text};
use iced_winit::{App, Application, Renderer};
/// The panel these tests simulate.
const SIZE: u32 = 200;

/// The container pads by 50 and the button fills what is left, so the button is exactly this.
fn button_bounds() -> Rectangle {
    Rectangle::new(Point::new(50.0, 50.0), Size::new(100.0, 100.0))
}

fn screen() -> Rectangle {
    Rectangle::with_size(Size::new(SIZE as f32, SIZE as f32))
}

#[derive(Debug, Clone)]
enum Message {
    Press,
}

/// Counts presses. Shared with the test, because `Application` takes ownership of the app.
struct Counter {
    presses: Rc<Cell<u32>>,
}

impl App for Counter {
    type Message = Message;
    type Theme = Theme;

    fn theme(&self) -> Theme {
        <Theme as Base>::default(Mode::Dark)
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::Press => self.presses.set(self.presses.get() + 1),
        }
    }

    fn view(&self) -> Element<'_, Message, Theme, Renderer> {
        container(
            button(text(self.presses.get().to_string()))
                .on_press(Message::Press)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(50)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

fn application() -> (Application<Counter>, Rc<Cell<u32>>) {
    let presses = Rc::new(Cell::new(0));

    let app = Application::new(
        Counter {
            presses: presses.clone(),
        },
        SIZE,
        SIZE,
    )
    .expect("buffers");

    (app, presses)
}

#[test]
fn an_idle_screen_is_never_drawn() {
    let (mut app, _) = application();

    // The first frame has nothing to diff against, so all of it is drawn.
    assert_eq!(app.frame(), vec![screen()]);

    // Nothing has changed since, so every later frame is free.
    assert!(
        app.frame().is_empty(),
        "an idle loop must not touch the panel"
    );
    assert!(app.frame().is_empty());
}

#[test]
fn a_tap_reaches_update_and_is_drawn_on_the_next_frame() {
    let (mut app, presses) = application();
    app.frame();

    let centre = button_bounds().center();
    app.touch_down(centre);
    app.touch_up();

    // The frame that carries the input produces the message, which is applied at the end of it.
    app.frame();
    assert_eq!(presses.get(), 1, "the tap reached `update`");

    // The label changed, so the frame after that has something to draw...
    let damaged = app.frame();
    assert!(
        !damaged.is_empty(),
        "the new label has to be drawn, got {damaged:?}"
    );

    // ...and then the screen is idle again.
    assert!(app.frame().is_empty());
}

#[test]
fn a_touch_outside_the_button_is_not_a_press() {
    let (mut app, presses) = application();
    app.frame();

    // Inside the container's padding, nowhere near the button.
    app.touch_down(Point::new(10.0, 10.0));
    app.touch_up();

    app.frame();
    assert_eq!(presses.get(), 0, "the hit test has to reject the padding");

    // And an idle frame stays idle.
    assert!(app.frame().is_empty());
}

#[test]
fn a_drag_that_ends_outside_does_not_press() {
    let (mut app, presses) = application();
    app.frame();

    // Press inside, drag out, release: iced's buttons only fire when the release is inside.
    app.touch_down(button_bounds().center());
    app.touch_move(Point::new(10.0, 10.0));
    app.touch_up();

    app.frame();
    assert_eq!(presses.get(), 0, "the release was outside the button");
}
