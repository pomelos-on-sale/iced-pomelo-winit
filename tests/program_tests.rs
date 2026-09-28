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
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use iced_core::theme::{Base, Mode};
use iced_core::window;
use iced_core::{Element, Length, Settings, Theme};
use iced_futures::backend::null;
use iced_program::Program;
use iced_runtime::Task;
use iced_widget::{container, text};
use iced_winit::{Application, ProgramApp, Renderer};

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

fn application() -> (Application<ProgramApp<Counting>>, Rc<RefCell<Vec<Message>>>) {
    let app = Counting::default();
    let seen = app.seen.clone();

    (
        Application::new(ProgramApp::new(app), SIZE, SIZE).expect("buffers"),
        seen,
    )
}

/// Runs frames until the app stops producing them, and returns how many produced damage.
///
/// The loop is what the firmware does: a frame, then the panel if anything changed.
fn run(app: &mut Application<ProgramApp<Counting>>) -> usize {
    let mut painted = 0;

    for _ in 0..20 {
        if app.frame().is_empty() && painted > 0 {
            break;
        }

        painted += 1;
    }

    painted
}

#[test]
fn a_programs_boot_task_is_applied_without_a_touch() {
    let (mut app, seen) = application();

    // The boot task is `Task::done`, which is ready in place: iced's own loop applies it in the
    // frame that asked for it, and so does this one.
    run(&mut app);

    assert_eq!(*seen.borrow(), vec![Message::Started, Message::Finished]);
}

#[test]
fn a_task_that_is_not_ready_finishes_on_a_later_frame() {
    let (mut app, seen) = application();

    // The first frame applies `Started`, which returns `Later(3)`; the next frames are what poll
    // it. Nothing else is driving the app: no touch, no widget, no timer.
    run(&mut app);

    assert!(
        seen.borrow().contains(&Message::Finished),
        "the executor did not run the task the program returned"
    );
}

#[test]
fn the_frame_after_the_message_arrives_is_painted() {
    let (mut app, _) = application();

    // The first frame applies boot's message and paints the label "1".
    assert!(!app.frame().is_empty());

    // The executor runs the rest of the chain, and the label it produces is "2" -- so at least one
    // of the frames after this one has to paint. Nothing asks for it: no touch, no widget.
    let painted = (0..6).any(|_| !app.frame().is_empty());

    assert!(
        painted,
        "the panel was never repainted after the task's message arrived"
    );
}

#[test]
fn the_panel_keeps_being_asked_for_frames_only_while_work_is_outstanding() {
    let (mut app, _) = application();

    run(&mut app);

    // The task is done and there is no subscription, so nothing is waiting on the executor and a
    // frame from here on is the loop asking for one, not the app needing one.
    assert!(!app.app().is_working());
    assert!(app.frame().is_empty());
}
