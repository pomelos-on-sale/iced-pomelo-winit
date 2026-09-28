//! The loop, driven the way the firmware drives it: a board, a program, and frames.
//!
//! `Host` is `run` with the board handed over instead of looked up, which is what makes the entry
//! point iced's facade calls testable: a fake board answers touch by itself, counts the frames it
//! was handed, and stops after a while so that `run` can return.
//!
//! What is being asserted here is the *loop*: that a touch reaches the program, that the panel is
//! only written when something changed, and that an idle screen is where the board is allowed to
//! sleep. Whether a `Program` runs at all is `program_tests.rs`'s job.

#![cfg(feature = "renderer")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iced_core::theme::{Base, Mode};
use iced_core::window;
use iced_core::{Element, Length, Point, Rectangle, Settings, Theme};
use iced_futures::backend::null;
use iced_program::Program;
use iced_runtime::Task;
use iced_widget::{button, container};
use iced_winit::{Board, Host};

/// The panel these tests simulate.
const SIZE: u32 = 200;

/// The button the container's padding leaves exactly this big, as in `application_tests.rs`.
fn button_bounds() -> Rectangle {
    Rectangle::new(Point::new(50.0, 50.0), iced_core::Size::new(100.0, 100.0))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Message {
    Press,
}

/// A program that counts presses, so the test can see what the loop delivered.
#[derive(Clone, Default)]
struct Counting {
    presses: Rc<RefCell<u32>>,
}

impl Program for Counting {
    type State = ();
    type Message = Message;
    type Theme = Theme;
    type Renderer = iced_winit::Renderer;
    type Executor = null::Executor;

    fn name() -> &'static str {
        "counting"
    }

    fn settings(&self) -> Settings {
        Settings::default()
    }

    fn window(&self) -> Option<window::Settings> {
        None
    }

    fn boot(&self) -> (Self::State, Task<Self::Message>) {
        ((), Task::none())
    }

    fn update(&self, _state: &mut Self::State, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Press => *self.presses.borrow_mut() += 1,
        }

        Task::none()
    }

    fn view<'a>(
        &self,
        _state: &'a Self::State,
        _window: window::Id,
    ) -> Element<'a, Self::Message, Self::Theme, Self::Renderer> {
        container(
            button(iced_widget::text("press"))
                .on_press(Message::Press)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(50)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn theme(&self, _state: &Self::State, _window: window::Id) -> Option<Theme> {
        Some(<Theme as Base>::default(Mode::Dark))
    }
}

/// A board that presses the panel twice and then leaves it alone, stopping after `frames`
/// iterations so that `run` can return.
///
/// The counters are `Cell`s because the trait takes `&self` for the things a real board reads
/// through FFI (`touch`, `running`) — a scripted board has to write down what it did.
struct Fake {
    /// How many times the loop has asked where the finger is.
    step: Cell<u32>,
    /// How many iterations to run before the loop should end.
    frames: u32,
    /// How many frames were handed to the panel.
    painted: Cell<u32>,
    /// How many times the loop was told the screen was idle.
    idled: Cell<u32>,
}

impl Fake {
    fn new(frames: u32) -> Self {
        Self {
            step: Cell::new(0),
            frames,
            painted: Cell::new(0),
            idled: Cell::new(0),
        }
    }
}

impl Board for Fake {
    fn size(&self) -> (u32, u32) {
        (SIZE, SIZE)
    }

    fn touch(&self) -> Option<Point> {
        let step = self.step.get();
        self.step.set(step + 1);

        // Down for two iterations and then gone: a press, a resting report, and a release, which
        // is what the loop has to translate into a click.
        (step < 2).then(|| button_bounds().center())
    }

    fn flush(&mut self, _damage: &[Rectangle], _panel: &[u16]) {
        self.painted.set(self.painted.get() + 1);
    }

    fn idle(&mut self) {
        self.idled.set(self.idled.get() + 1);
    }

    fn running(&self) -> bool {
        self.step.get() < self.frames
    }
}

/// Runs the loop for `frames` iterations and hands back what the program saw.
#[test]
fn a_touch_reaches_the_program_through_the_widget_tree() {
    let app = Counting::default();
    let presses = Rc::clone(&app.presses);

    let mut host = Host::new(app, Box::new(Fake::new(6))).expect("buffers");

    host.run().expect("the loop runs");

    assert_eq!(
        *presses.borrow(),
        1,
        "a press and a release through the loop is one click"
    );
}

#[test]
fn an_idle_screen_is_where_the_board_may_sleep() {
    let mut host = Host::new(Counting::default(), Box::new(Fake::new(6))).expect("buffers");

    // The first iteration paints; the ones after it have nothing to draw, and that is where a
    // board gets to wait instead of spinning.
    assert!(host.step(), "the first frame is the panel's initial paint");
    assert!(!host.step(), "nothing changed, so nothing was written");
}

#[test]
fn run_without_a_board_is_an_error_rather_than_a_panic() {
    let error = iced_winit::run(Counting::default()).expect_err("no board was registered");

    assert!(matches!(error, iced_winit::Error::WindowCreationFailed(_)));
}
