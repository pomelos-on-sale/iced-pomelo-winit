//! Running an iced [`Program`]: the entry point an app written for iced already has.
//!
//! [`crate::App`] is this stack's own shape — no state split, no tasks, no window id — and it is
//! what the seven apps in this OS are written against. A [`Program`] is iced's shape, and an app
//! that implements it runs here *and* on a desktop, unchanged: the only difference is who calls
//! it (the firmware's loop here, `iced::application(..).run()` there).
//!
//! [`ProgramApp`] is that adapter. It owns the [`Instance`] (the program together with its
//! state), an [`iced_futures::Runtime`] for tasks and subscriptions, and the [`Pump`] those run
//! on, and it presents all of that to the loop as an ordinary [`crate::App`]:
//!
//! * `update` runs the program's `update`, then polls the [`Task`] it returned — immediately
//!   ready actions are applied in the same frame, exactly as iced's own loop does it — and hands
//!   whatever is still pending to the runtime;
//! * `subscription` is asked for after every update and handed to the runtime's tracker, which
//!   spawns and drops the streams as the recipes change;
//! * `poll` advances the executor, then turns what came back into messages.
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
use crate::{App, Renderer};

/// How many actions may be waiting for the loop to collect them.
///
/// A task chain delivers one action per step and the loop drains them every frame, so this is a
/// ceiling on a burst, not on the work an app can do.
const ACTIONS: usize = 64;

/// An iced [`Program`], hosted by this stack's loop.
pub struct ProgramApp<P: Program> {
    instance: Instance<P>,
    runtime: Runtime<Pump, mpsc::Sender<Action<P::Message>>, Action<P::Message>>,
    actions: mpsc::Receiver<Action<P::Message>>,
    pump: Pump,
    /// The one window: the panel.
    window: window::Id,
    /// Actions a task delivered without waiting. iced's own loop applies these in the frame that
    /// asked for them rather than handing them to the executor, and so does this one.
    immediate: Vec<Action<P::Message>>,
}

impl<P: Program> ProgramApp<P> {
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

        app
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

    /// Runs a task: the part that is ready now, and the rest on the executor.
    ///
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

impl<P> App for ProgramApp<P>
where
    P: Program<Renderer = Renderer>,
{
    type Message = P::Message;
    type Theme = P::Theme;

    fn theme(&self) -> P::Theme {
        // `None` is the program saying "the platform's choice", and this platform's default is
        // iced's own for a dark panel.
        self.instance
            .theme(self.window)
            .unwrap_or_else(|| <P::Theme as Base>::default(Mode::Dark))
    }

    fn update(&mut self, message: P::Message) {
        let task = self.runtime.enter(|| self.instance.update(message));

        self.run(task);
        self.track();
    }

    fn view(&self) -> Element<'_, P::Message, P::Theme, Renderer> {
        self.instance.view(self.window)
    }

    fn poll(&mut self) -> Vec<P::Message> {
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
