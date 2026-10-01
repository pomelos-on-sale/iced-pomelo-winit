//! Running an iced [`Program`]: the entry point an app written for iced already has.
//!
//! [`crate::App`] is this stack's own shape — no state split, no tasks, no window id — and it is
//! what the seven apps in this OS are written against. A [`Program`] is iced's shape, and an app
//! that implements it runs here *and* on a desktop, unchanged: the only difference is who calls
//! it (the firmware's loop here, `iced::application(..).run()` there).
//!
//! [`ProgramApp`] is the adapter. It owns the [`Instance`] (the program together with its
//! state), an [`iced_futures::Runtime`] for tasks and subscriptions, and the [`Pump`] those run
//! on, and it hands the loop the four things a frame needs — the view, the theme, an update, and
//! whatever popped out of the executor:
//!
//! * `update` runs the program's `update`, then polls the [`Task`] it returned — immediately
//!   ready actions are applied in the same frame, exactly as iced's own loop does it — and hands
//!   whatever is still pending to the runtime;
//! * `subscription` is asked for after every update and handed to the runtime's tracker, which
//!   spawns and drops the streams as the recipes change -- and, like iced's own loop, once at boot
//!   before anything has been updated;
//! * `poll` advances the executor, then turns what came back into messages;
//! * `broadcast` feeds the runtime's event stream, which is what the window subscriptions listen
//!   to. The events are the loop's to send, because they are the loop's to know: see [`crate::Host`].
//!
//! # Two deliberate differences from a desktop
//!
//! **The program's `Executor` type is not used.** iced asks the app which executor to run its
//! futures on, because on a desktop the answer is a choice (tokio, smol, a thread pool) — and the
//! default on a platform without one is `iced_futures`' *null* backend, which drops every future
//! on the floor. An app that names the default would therefore lose every task silently. So the
//! host names [`Pump`] itself and the app's choice stays a formality, which is also what keeps the
//! app portable: `type Executor = iced::executor::Default` compiles here and means something
//! there.
//!
//! **There is one window, and it is the panel.** `window::Settings` is read for nothing,
//! `window::Id` is minted once and never changes, `title` has no title bar to reach, and the
//! window actions (`open`, `close`, `resize`) have nothing to act on. A `Program` that asks for a
//! second window is asking for something this hardware does not have.
//!
//! # Not done yet
//!
//! Widget operations (`Action::Widget`) are dropped instead of applied to the tree. iced uses them
//! for things like focusing a text input from a task, and applying one means calling
//! `UserInterface::operate`, which the loop owns and this module cannot reach. Until then,
//! `Task::widget` and the operations behind `text_input::focus` do nothing on this platform.
//!
//! # Why this is behind the `renderer` feature
//!
//! `Program`'s renderer has to be one that can be built *headless* and can name a compositor —
//! `iced_program::Renderer` is that bound — because on a desktop those two are what create a
//! window and its surface. In this stack only `iced-pomelo-gfx` implements them, and that is the
//! crate behind the `renderer` feature; `iced_tiny_skia` has neither without its softbuffer-based
//! window compositor, which does not build here. So hosting a `Program` is a recorded-renderer
//! feature until the recorded renderer is the only path, and then the gate disappears with the
//! rest of them.

use std::borrow::Cow;
use std::task::{Context, Poll};

use iced_core::theme::{Base, Mode};
use iced_core::window;
use iced_core::Element;
use iced_futures::futures::channel::mpsc;
use iced_futures::futures::task::noop_waker_ref;
use iced_futures::futures::StreamExt as _;
use iced_futures::{subscription, Runtime};
use iced_program::{Instance, Program};
use iced_runtime::{task, Action, Task};

use crate::executor::Pump;

/// How many actions may be waiting for the loop to collect them.
///
/// A task chain delivers one action per step and the loop drains them every frame, so this is a
/// ceiling on a burst, not on the work an app can do.
const ACTIONS: usize = 64;

/// An iced [`Program`], hosted by this stack's loop.
pub struct ProgramApp<P: Program> {
    instance: Instance<P>,
    runtime: ProgramRuntime<P>,
    actions: mpsc::Receiver<Action<P::Message>>,
    pump: Pump,
    /// The one window: the panel.
    window: window::Id,
    /// Actions a task delivered without waiting. iced's own loop applies these in the frame that
    /// asked for them rather than handing them to the executor, and so does this one.
    immediate: Vec<Action<P::Message>>,
}

/// The runtime a hosted program's tasks are tracked by: this stack's [`Pump`], and a channel of
/// [`Action`]s back to the loop. Named rather than spelled out in the field, because the spelled-out
/// version is genuinely hard to read — which is what clippy says.
type ProgramRuntime<P> =
    Runtime<Pump, mpsc::Sender<Action<<P as Program>::Message>>, Action<<P as Program>::Message>>;

impl<P> ProgramApp<P>
where
    P: Program,
{
    /// Boots `program` and takes ownership of it, running the task its `boot` returned.
    pub fn new(program: P) -> Self {
        let pump = Pump::new();
        let (sender, actions) = mpsc::channel(ACTIONS);
        let runtime = Runtime::new(pump.clone(), sender);
        let (instance, task) = Instance::new(program);

        let mut app = Self {
            instance,
            runtime,
            actions,
            pump,
            window: window::Id::unique(),
            immediate: Vec::new(),
        };

        app.run(task);

        // The subscription the program had at boot, before anything has been updated. iced's own
        // loop does the same right after boot, and the difference is visible: a program whose only
        // subscription is `window::resize_events()` -- which is how an app is told how big its
        // screen is -- would otherwise be silent until its first message, which is the very
        // message it is waiting for.
        app.track();

        app
    }

    /// Hands a window event to whatever is subscribed to the window's events.
    ///
    /// This is the half of iced's contract that `iced_winit` supplies on a desktop and that this
    /// shell has to supply itself, because the panel *is* the window: `window::resize_events()`,
    /// `window::frames()`, `window::events()` and `keyboard::listen` are all listeners on the
    /// runtime's event stream, and a shell that never sends an event leaves every one of them
    /// permanently silent. What iced_winit broadcasts is the list of events a real window
    /// produces; what this shell can honestly produce is the list in [`crate::Host`]'s
    /// documentation -- a size, and "a frame happened".
    pub fn broadcast_event(&mut self, event: iced_core::Event) {
        self.runtime.broadcast(subscription::Event::Interaction {
            window: self.window,
            event,
            status: iced_core::event::Status::Ignored,
        });
    }

    pub fn broadcast(&mut self, event: window::Event) {
        self.broadcast_event(iced_core::Event::Window(event));
    }

    /// The executor the program's tasks run on, for a host that wants to watch it.
    pub fn pump(&self) -> Pump {
        self.pump.clone()
    }

    /// Whether the program has work outstanding: a task that has not finished, or a subscription
    /// that is alive and being waited on.
    pub fn is_working(&self) -> bool {
        self.pump.pending() > 0
    }

    /// The title the program reports. There is no title bar; this is for a log.
    pub fn title(&self) -> String {
        self.instance.title(self.window)
    }

    /// Runs a task: the part that is ready now, and the rest on the executor.    ///
    /// The split is iced's own (`iced_winit::update`): a `Task::done` or a widget operation is
    /// ready the first time it is polled, and routing it through the executor would delay every
    /// one of them to the next frame.
    fn run(&mut self, task: Task<P::Message>) {
        let Some(mut stream) = task::into_stream(task) else {
            return;
        };

        let waker = noop_waker_ref();
        let mut context = Context::from_waker(waker);

        loop {
            match self.runtime.enter(|| stream.poll_next_unpin(&mut context)) {
                Poll::Ready(Some(action)) => self.immediate.push(action),
                Poll::Ready(None) => break,
                Poll::Pending => {
                    self.runtime.run(stream);
                    break;
                }
            }
        }
    }

    /// Hands the program's current subscription to the runtime's tracker.
    fn track(&mut self) {
        let subscription = self.runtime.enter(|| self.instance.subscription());

        self.runtime
            .track(subscription::into_recipes(subscription.map(Action::Output)));
    }

    /// Turns one action into a message, or does what it asks for.
    fn perform(&mut self, action: Action<P::Message>) -> Option<P::Message> {
        match action {
            Action::Output(message) => Some(message),

            // A font loaded at runtime. Same install the boot path uses with the embedded
            // subset, and then the reply the task is waiting on.
            Action::LoadFont { bytes, channel } => {
                // `install` borrows the bytes for the life of the program, which for a font is
                // the life of the process; the leak is the price of that, and fonts are few.
                let bytes: &'static [u8] = match bytes {
                    Cow::Borrowed(bytes) => bytes,
                    Cow::Owned(bytes) => Box::leak(bytes.into_boxed_slice()),
                };

                crate::fonts::install(bytes);

                let _ = channel.send(Ok(()));

                None
            }

            // Everything else is a desktop platform action: a widget operation reaches into a tree
            // this module does not own (see the module docs), and the clipboard, window, system
            // and image actions have nothing to act on here -- one panel, no window manager, no
            // clipboard, no system theme. `Reload` rebuilds every window, `Exit` ends a process.
            // Ignoring them is a decision rather than an oversight, which is why they are listed.
            Action::Widget(_)
            | Action::Clipboard(_)
            | Action::Window(_)
            | Action::System(_)
            | Action::Image(_)
            | Action::Reload
            | Action::Exit => None,
        }
    }
}

/// What the loop asks of it, one frame at a time.
///
/// Not an `impl App`: an [`App`](crate::App) here names its renderer concretely, and the whole
/// point of hosting a `Program` is that the renderer comes from the program — iced's facade hands
/// its shell a program whose `Renderer` is a type *parameter*, so the shell cannot name it either.
/// The loop that drives this type is generic over `P: Program` for that reason, and reaches the
/// renderer through the compositor that `P::Renderer` names.
impl<P> ProgramApp<P>
where
    P: Program,
{
    /// The theme in effect for the one window.
    ///
    /// `None` is the program saying "the platform's choice", and this platform's default is iced's
    /// own for a dark panel.
    pub fn theme(&self) -> P::Theme
    where
        P::Theme: Base,
    {
        self.instance
            .theme(self.window)
            .unwrap_or_else(|| <P::Theme as Base>::default(Mode::Dark))
    }

    /// A message for the program, and whatever work it produced.
    pub fn update(&mut self, message: P::Message) {
        let task = self.runtime.enter(|| self.instance.update(message));

        self.run(task);
        self.track();
    }

    /// The state the program is in.
    ///
    /// For the loop around it and for its tests: a host that cannot see what it is hosting cannot
    /// report on it, and this is the same read-only door iced's own `Instance` now has.
    pub fn state(&self) -> &P::State {
        self.instance.state()
    }

    /// What the program wants drawn for the one window.
    pub fn view(&self) -> Element<'_, P::Message, P::Theme, P::Renderer> {
        self.instance.view(self.window)
    }

    pub fn poll(&mut self) -> Vec<P::Message> {
        self.pump.tick();

        let mut messages = Vec::new();

        for action in std::mem::take(&mut self.immediate) {
            if let Some(message) = self.perform(action) {
                messages.push(message);
            }
        }

        // One frame's worth of the executor's output. `try_recv` is the non-blocking read: the
        // channel is fed by futures polled a line above, and nothing else can be waiting. An
        // empty or closed channel is both an error and both mean "nothing more this frame".
        while let Ok(action) = self.actions.try_recv() {
            if let Some(message) = self.perform(action) {
                messages.push(message);
            }
        }

        messages
    }
}
