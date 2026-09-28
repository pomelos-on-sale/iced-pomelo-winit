//! The window events the loop produces, and what a program subscribed to them hears.
//!
//! A program written for iced can ask to be told about its window: `window::frames()` is how an
//! animation learns that a frame happened, and `window::resize_events()` is how anything that lays
//! itself out learns how big the screen is. On a desktop `iced_winit` sends both — it broadcasts
//! every event a real window produces into the runtime, and that stream is what those subscriptions
//! listen to. On this hardware there is no winit, so the loop has to send them itself, and what it
//! can honestly send is short: the panel's size once, and "a frame happened" after every frame.
//!
//! These tests are about exactly that, and about the fact that a subscription which is merely
//! *waiting* must not cost frames.

#![cfg(feature = "renderer")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iced_core::theme::{Base, Mode};
use iced_core::window;
use iced_core::{Element, Length, Point, Rectangle, Settings, Size, Theme};
use iced_futures::backend::null;
use iced_futures::Subscription;
use iced_program::Program;
use iced_runtime::Task;
use iced_widget::{container, text};
use iced_winit::{Board, Host};

/// The panel these tests simulate.
const SIZE: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Message {
    /// The loop drew a frame.
    Frame,
    /// The loop told the program how big its window is.
    Resized(Size),
}

/// A program whose subscription is `window::frames()`: one message per frame the loop draws.
#[derive(Clone, Default)]
struct Frames {
    frames: Rc<Cell<u32>>,
}

impl Program for Frames {
    type State = Rc<Cell<u32>>;
    type Message = Message;
    type Theme = Theme;
    type Renderer = iced_winit::Renderer;
    type Executor = null::Executor;

    fn name() -> &'static str {
        "frames"
    }

    fn settings(&self) -> Settings {
        Settings::default()
    }

    fn window(&self) -> Option<window::Settings> {
        None
    }

    fn boot(&self) -> (Self::State, Task<Self::Message>) {
        (self.frames.clone(), Task::none())
    }

    fn update(&self, state: &mut Self::State, message: Self::Message) -> Task<Self::Message> {
        if let Message::Frame = message {
            state.set(state.get() + 1);
        }

        Task::none()
    }

    fn view<'a>(
        &self,
        state: &'a Self::State,
        _window: window::Id,
    ) -> Element<'a, Self::Message, Self::Theme, Self::Renderer> {
        // The count is drawn, so every frame damages the panel: what is being asserted is that the
        // frames keep arriving without anything else driving them.
        container(text(state.get().to_string()))
            .padding(50)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self, _state: &Self::State) -> Subscription<Self::Message> {
        iced_runtime::window::frames().map(|_| Message::Frame)
    }

    fn theme(&self, _state: &Self::State, _window: window::Id) -> Option<Theme> {
        Some(<Theme as Base>::default(Mode::Dark))
    }
}

/// A program whose subscription is `window::resize_events()`: told the size, and then silent.
#[derive(Clone, Default)]
struct Sized {
    sizes: Rc<RefCell<Vec<Size>>>,
}

impl Program for Sized {
    type State = Rc<RefCell<Vec<Size>>>;
    type Message = Message;
    type Theme = Theme;
    type Renderer = iced_winit::Renderer;
    type Executor = null::Executor;

    fn name() -> &'static str {
        "sized"
    }

    fn settings(&self) -> Settings {
        Settings::default()
    }

    fn window(&self) -> Option<window::Settings> {
        None
    }

    fn boot(&self) -> (Self::State, Task<Self::Message>) {
        (self.sizes.clone(), Task::none())
    }

    fn update(&self, state: &mut Self::State, message: Self::Message) -> Task<Self::Message> {
        if let Message::Resized(size) = message {
            state.borrow_mut().push(size);
        }

        Task::none()
    }

    fn view<'a>(
        &self,
        _state: &'a Self::State,
        _window: window::Id,
    ) -> Element<'a, Self::Message, Self::Theme, Self::Renderer> {
        container(text("sized"))
            .padding(50)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self, _state: &Self::State) -> Subscription<Self::Message> {
        iced_runtime::window::resize_events().map(|(_window, size)| Message::Resized(size))
    }

    fn theme(&self, _state: &Self::State, _window: window::Id) -> Option<Theme> {
        Some(<Theme as Base>::default(Mode::Dark))
    }
}

/// A board with no fingers, counting the frames the panel was written in.
#[derive(Default)]
struct Panel {
    painted: Cell<usize>,
}

impl Board for Panel {
    fn size(&self) -> (u32, u32) {
        (SIZE, SIZE)
    }

    fn touch(&self) -> Option<Point> {
        None
    }

    fn flush(&mut self, _damage: &[Rectangle], _pixels: &[u16]) {
        self.painted.set(self.painted.get() + 1);
    }

    fn idle(&mut self) {}
}

fn host<P: Program + 'static>(program: P) -> Host<P> {
    Host::new(program, Box::new(Panel::default())).expect("a panel and its buffers")
}

#[test]
fn a_frame_is_an_event_a_program_can_subscribe_to() {
    let program = Frames::default();
    let frames = program.frames.clone();
    let mut host = host(program);

    // One message per frame, and it arrives in the frame that produced it: the loop polls the
    // executor once more at the end of a frame, which is where a message that came out of the
    // frame's own work is applied (iced's own loop does the same, for tasks).
    for expected in 1..=5 {
        assert!(host.step(), "frame {expected} paints");
        assert_eq!(frames.get(), expected, "one message per frame");
    }

    // Which is also why the program's animation keeps going with nothing else driving it: the
    // reaction to a frame is what makes the next one. That *is* `frames()`.
    assert!(
        host.step(),
        "the panel is still being painted, six frames in, with no touch and no task"
    );
}

#[test]
fn a_programs_window_is_told_its_size() {
    let program = Sized::default();
    let sizes = program.sizes.clone();
    let mut host = host(program);

    // In the frame the program is first asked what it wants, it is told -- not on some later frame,
    // and not never, which is what a shell that never broadcasts produces.
    assert!(host.step(), "the first frame paints");

    assert_eq!(
        *sizes.borrow(),
        vec![Size::new(SIZE as f32, SIZE as f32)],
        "the panel's size, once"
    );

    host.step();

    assert_eq!(
        sizes.borrow().len(),
        1,
        "a panel cannot be resized, so it is told once"
    );
}

/// A subscription that is waiting is not work.
///
/// The executor counts a live subscription as a future of its own, so "is the program working"
/// is true for the whole life of one -- and if the loop asked *that* before deciding to draw, a
/// program waiting for its size would be handed a frame a thousand times a second (the firmware's
/// loop runs at 1 kHz) to draw the same picture. What makes a frame is a message, a touch, or a
/// widget asking for one.
#[test]
fn a_subscription_with_nothing_to_say_costs_no_frames() {
    let program = Sized::default();
    let sizes = program.sizes.clone();
    let mut host = host(program);

    assert!(host.step(), "the first frame paints");
    assert_eq!(sizes.borrow().len(), 1, "and the size arrived with it");

    // The subscription is alive and waiting, which is not a reason to draw -- and *is* what the
    // executor reports as outstanding work, so this is the case that used to matter.
    assert!(
        host.app().is_working(),
        "the subscription is still alive, which is exactly the case that must not cost frames"
    );

    // One more frame is owed to the message (its effect has to be drawn), and its view is the same
    // picture, so the panel is not written. After that the loop does not even build a tree.
    for _ in 0..10 {
        assert!(!host.step(), "an idle program gets no frames");
    }
}
