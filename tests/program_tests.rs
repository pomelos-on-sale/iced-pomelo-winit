//! Running an iced `Program`: the entry point an app written for iced already has.
//!
//! The app here is written the way a desktop iced app is written -- `Program`, a state that is
//! separate from the program, messages that can come from a task -- and the only thing that makes
//! it run on this board is that the host knows how to drive one.
//!
//! Two things are being asserted, and they are different:
//!
//! * a message from a task is applied, without a widget or a touch being involved, and its effect
//!   is on the panel;
//! * a task that is *not* ready when it is first polled still finishes, which is what the
//!   executor is for — the app below names iced's `null` executor, which drops every future, so
//!   nothing would ever arrive if the host used it.
//!
//! `Program` needs a renderer that can be built headless, so this runs on the recorded renderer:
//! see the `program` module.

#![cfg(feature = "renderer")]
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use iced_core::theme::{self, Base, Mode};
use iced_core::window;
use iced_core::{Element, Length, Point, Rectangle, Settings, Theme};
use iced_futures::backend::null;
use iced_futures::Subscription;
use iced_program::Program;
use iced_runtime::Task;
use iced_widget::{container, text};
use iced_winit::{Board, Host, Renderer};

/// The panel these tests simulate.
const SIZE: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Message {
    /// Boot's own message.
    Started,
    /// The one the task below brings.
    Finished,
}

/// A future that is not ready the first time it is polled.
///
/// `Task::perform` hands it to the executor, and the executor polls it once per frame; a future
/// that resolves on the first poll would not prove that anything was running it.
struct Later(u8);

impl Future for Later {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        self.0 -= 1;

        if self.0 == 0 {
            Poll::Ready(())
        } else {
            context.waker().wake_by_ref();

            Poll::Pending
        }
    }
}

/// An app in iced's shape: no widget is involved in either of its messages.
///
/// The state is shared with the test rather than read back out of the host, because the host owns
/// it once the app is inside an `Application` -- the same reason `application_tests.rs` shares a
/// press counter.
#[derive(Clone, Default)]
struct Counting {
    seen: Rc<RefCell<Vec<Message>>>,
}

impl Program for Counting {
    type State = Rc<RefCell<Vec<Message>>>;
    type Message = Message;
    type Theme = Theme;
    type Renderer = Renderer;
    /// Deliberately iced's null backend: it drops every future it is given, so if the host
    /// honoured this, `Finished` would never arrive. See `program.rs`.
    type Executor = null::Executor;

    fn name() -> &'static str {
        "counting"
    }

    fn settings(&self) -> Settings {
        Settings::default()
    }

    fn window(&self) -> Option<window::Settings> {
        // One window, and it is the panel: the host mints the id and ignores this.
        None
    }

    fn boot(&self) -> (Self::State, Task<Self::Message>) {
        (self.seen.clone(), Task::done(Message::Started))
    }

    fn update(&self, state: &mut Self::State, message: Self::Message) -> Task<Self::Message> {
        state.borrow_mut().push(message);

        match message {
            // Boot's message asks for a second one to arrive asynchronously.
            Message::Started => Task::perform(Later(3), |()| Message::Finished),
            Message::Finished => Task::none(),
        }
    }

    fn view<'a>(
        &self,
        state: &'a Self::State,
        _window: window::Id,
    ) -> Element<'a, Self::Message, Self::Theme, Self::Renderer> {
        container(text(state.borrow().len().to_string()))
            .padding(50)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn theme(&self, _state: &Self::State, _window: window::Id) -> Option<Theme> {
        Some(<Theme as Base>::default(Mode::Dark))
    }
}

/// The shape iced's own facade puts a `Program` in.
///
/// `iced::Application<P>` is a wrapper generic over `P::Renderer` that forwards every method to
/// `P` and hands *itself* to `iced_winit::run` — and its `Renderer` is `<P as Program>::Renderer`,
/// an associated type nothing else knows anything about. So a shell that asked for one particular
/// renderer could not accept it, which is exactly why this crate's `run` asks for none. Compiling
/// this type is a third of what these tests assert; running it is another.
struct Wrapper<P: Program>(P);

impl<P: Program> Program for Wrapper<P> {
    type State = P::State;
    type Message = P::Message;
    type Theme = P::Theme;
    type Renderer = P::Renderer;
    type Executor = P::Executor;

    fn name() -> &'static str {
        P::name()
    }

    fn settings(&self) -> Settings {
        self.0.settings()
    }

    fn window(&self) -> Option<window::Settings> {
        self.0.window()
    }

    fn boot(&self) -> (Self::State, Task<Self::Message>) {
        self.0.boot()
    }

    fn update(&self, state: &mut Self::State, message: Self::Message) -> Task<Self::Message> {
        self.0.update(state, message)
    }

    fn view<'a>(
        &self,
        state: &'a Self::State,
        window: window::Id,
    ) -> Element<'a, Self::Message, Self::Theme, Self::Renderer> {
        self.0.view(state, window)
    }

    fn title(&self, state: &Self::State, window: window::Id) -> String {
        self.0.title(state, window)
    }

    fn subscription(&self, state: &Self::State) -> Subscription<Self::Message> {
        self.0.subscription(state)
    }

    fn theme(&self, state: &Self::State, window: window::Id) -> Option<Self::Theme> {
        self.0.theme(state, window)
    }

    fn style(&self, state: &Self::State, theme: &Self::Theme) -> theme::Style {
        self.0.style(state, theme)
    }

    fn scale_factor(&self, state: &Self::State, window: window::Id) -> f32 {
        self.0.scale_factor(state, window)
    }
}

/// A board with no fingers.
///
/// These tests are about the loop's other half: work that arrives from a task instead of from a
/// touch. The board still has to exist — the loop asks it where the finger is, every iteration —
/// and it is where the panel's flushes are counted.
#[derive(Default)]
struct Panel {
    painted: Cell<usize>,
    idled: Cell<usize>,
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

    fn idle(&mut self) {
        self.idled.set(self.idled.get() + 1);
    }
}

fn host<P: Program + 'static>(program: P) -> Host<P> {
    Host::new(program, Box::new(Panel::default())).expect("a panel and its buffers")
}

/// Runs steps until the program stops producing them, and returns how many painted.
///
/// The loop is what the firmware does: a step, and the panel only if something changed.
fn run<P: Program + 'static>(host: &mut Host<P>) -> usize {
    let mut painted = 0;

    for _ in 0..20 {
        if !host.step() && painted > 0 {
            break;
        }

        painted += 1;
    }

    painted
}

#[test]
fn a_programs_boot_task_is_applied_without_a_touch() {
    let program = Counting::default();
    let seen = program.seen.clone();
    let mut host = host(program);

    // The boot task is `Task::done`, which is ready in place: iced's own loop applies it in the
    // frame that asked for it, and so does this one.
    run(&mut host);

    assert_eq!(*seen.borrow(), vec![Message::Started, Message::Finished]);
}

#[test]
fn a_task_that_is_not_ready_finishes_on_a_later_frame() {
    let program = Counting::default();
    let seen = program.seen.clone();
    let mut host = host(program);

    // The first frame applies `Started`, which returns `Later(3)`; the next frames are what poll
    // it. Nothing else is driving the program: no touch, no widget, no timer.
    run(&mut host);

    assert!(
        seen.borrow().contains(&Message::Finished),
        "the executor did not run the task the program returned"
    );
}

#[test]
fn the_frame_after_the_message_arrives_is_painted() {
    let mut host = host(Counting::default());

    // The first frame applies boot's message and paints the label "1".
    assert!(host.step(), "the first frame is the panel's initial paint");

    // The executor runs the rest of the chain, and the label it produces is "2" -- so at least one
    // of the frames after this one has to paint. Nothing asks for it: no touch, no widget.
    let painted = (0..6).any(|_| host.step());

    assert!(
        painted,
        "the panel was never repainted after the task's message arrived"
    );
}

#[test]
fn the_panel_keeps_being_asked_for_frames_only_while_work_is_outstanding() {
    let mut host = host(Counting::default());

    run(&mut host);

    // The task is done and there is no subscription, so nothing is waiting on the executor and a
    // frame from here on is the loop asking for one, not the program needing one.
    assert!(!host.app().is_working());
    assert!(!host.step(), "nothing outstanding, so nothing is painted");
}

/// The facade's shape, actually hosted.
///
/// `iced::application(..).run()` ends in `iced_winit::run(program)` with the wrapped program, so
/// this test is that call with the wrapper standing in for the facade's: a program whose renderer
/// is an associated type the shell cannot name, driven to the panel.
#[test]
fn a_wrapper_generic_over_the_renderer_is_accepted() {
    let program = Counting::default();
    let seen = program.seen.clone();
    let mut host = host(Wrapper(program));

    run(&mut host);

    assert_eq!(*seen.borrow(), vec![Message::Started, Message::Finished]);
}
