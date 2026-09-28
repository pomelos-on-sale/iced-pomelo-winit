//! The event loop: input → update → view → present.
//!
//! iced's own loop is `iced_winit::run`, which needs winit and a window. Its shape is three
//! phases owned by `iced_runtime::UserInterface` — build the widget tree, feed it events, draw
//! it — plus two that belong to the platform: where the events come from, and where the pixels
//! go. Those two are the caller's, so what is left here is the loop and the translation.
//!
//! Nothing below knows about the panel's FFI. A board backend polls its touch controller, calls
//! [`Application::touch_down`] / [`touch_move`] / [`touch_up`], calls [`Application::frame`], and
//! hands the damaged rectangles and [`Application::panel`] to its LCD.

use iced_core::theme::Base as _;
use iced_core::{
    clipboard, mouse, renderer, theme, window, Color, Element, Event, Font, Pixels, Point,
    Rectangle, Size,
};
use iced_runtime::user_interface::{Cache, State, UserInterface};
use pomelo_gfx::Pixmap565;
use std::time::Instant;

use crate::Surface;

/// The renderer our apps are written against.
///
/// An app names this exactly once, in the `Element` it returns from [`App::view`]. It is
/// `iced-pomelo-gfx`'s renderer when the recorded path is on, and `iced_tiny_skia`'s otherwise --
/// the two are interchangeable from the app's side, which is what makes the switch a switch.
#[cfg(feature = "renderer")]
pub type Renderer = iced_pomelo_gfx::Renderer;

#[cfg(not(feature = "renderer"))]
pub type Renderer = iced_tiny_skia::Renderer;

/// An app: iced's vocabulary, minus everything that assumes there is a window.
///
/// This is deliberately not `iced_program::Program`, which is sized for a windowing backend —
/// it carries window settings, window ids, an async executor and subscriptions we cannot honor.
/// The traits that matter for drawing are `iced_runtime`'s, and they are satisfied by the
/// `Element` this returns.
pub trait App {
    /// The message type [`App::update`] accepts.
    type Message: Send + 'static;

    /// The theme, which supplies the default text and background colors.
    type Theme: theme::Base;

    /// The theme in effect.
    fn theme(&self) -> Self::Theme;

    /// Reacts to a message.
    fn update(&mut self, message: Self::Message);

    /// Describes the interface for the current state.
    fn view(&self) -> Element<'_, Self::Message, Self::Theme, Renderer>;

    /// Messages from work that is not the widget tree, collected once per frame.
    ///
    /// This is where a [`Program`](crate::ProgramApp)'s tasks and subscriptions arrive: the
    /// adapter advances its executor and hands back whatever it produced. It is asked before the
    /// frame's events and before the decision to skip the frame, because a message that arrives
    /// while nothing else is happening still has to be applied.
    ///
    /// The default is empty, so an app that has no such work pays one call that returns an empty
    /// `Vec` per frame.
    fn poll(&mut self) -> Vec<Self::Message> {
        Vec::new()
    }

    /// Whether the app is animating, and so wants a frame even when nothing has happened.
    ///
    /// The host asks this before deciding to skip a frame. Without it an animation would have to
    /// make its own frames, and there is nowhere for it to do that: the loop draws when the app is
    /// dirty or has events, and a clock ticking is neither. The apps these ports replaced had
    /// exactly this method, and their host loop polled it the same way.
    fn is_animating(&self) -> bool {
        false
    }

    /// Advances time-based state, once per frame, before [`App::view`] is asked for it.
    ///
    /// `view` takes `&self` -- iced's interface is a function of the app's state -- so an
    /// animation has to be advanced somewhere that can mutate, and this is that place. A `Program`
    /// would drive this from a subscription to a time stream; an [`App`] here is advanced by the
    /// loop's own clock, which is the same clock every frame is timed from.
    fn tick(&mut self) {}
}

/// A running [`App`]: the widget tree's cache, the renderer, the surface, and the pointer.
pub struct Application<A: App> {
    app: A,
    renderer: Renderer,
    surface: Surface,
    tree: Tree,
    messages: Vec<A::Message>,
    /// Whether a message was delivered since the last frame. A message is applied after the
    /// tree that produced it is gone, so its effect has to be drawn by the *next* frame; this
    /// is what makes that frame happen even though it carries no input.
    dirty: bool,
    /// Whether the interface itself asked for another frame — an animation in progress, a
    /// blinking cursor, scroll momentum. Recomputed every frame from what `update` reports.
    redraw_requested: bool,
}

/// The widget tree's side of a frame, with the app taken out of it.
///
/// Everything here is the same whether this crate is hosting an [`App`] — its own shape — or an
/// iced [`Program`](crate::ProgramApp): the events a platform delivers, the cursor, the
/// clipboard, the tree's cache, and the platform's half of iced's redraw contract. That last one
/// is the subtle part, and the reason this is shared rather than written twice: iced's widgets
/// decide what they look like when they see a redraw request (`Button::draw` paints
/// `self.status`, and `self.status` is written in exactly one place — the
/// `window::Event::RedrawRequested` arm), and there is no window here to send one.
///
/// What is *not* in here is everything that differs between the two shapes: who owns the renderer,
/// what a message means, whether there is an executor behind the app.
pub struct Tree {
    cache: Cache,
    cursor: mouse::Cursor,
    clipboard: clipboard::Null,
    events: Vec<Event>,
}

impl Tree {
    pub fn new() -> Self {
        Self {
            cache: Cache::new(),
            cursor: mouse::Cursor::Unavailable,
            clipboard: clipboard::Null,
            events: Vec::new(),
        }
    }

    /// Whether an event is waiting to be delivered.
    pub fn is_idle(&self) -> bool {
        self.events.is_empty()
    }

    /// Queues an event. For tests and for backends with more inputs than a touchscreen.
    pub fn push_event(&mut self, event: Event) {
        self.events.push(event);
    }

    /// A finger touched the panel at `point`.
    pub fn touch_down(&mut self, point: Point) {
        // A touchscreen has no hover, so the press is three things at once: the pointer appears,
        // it is here, and the left button goes down. This is the mapping iced_winit uses for
        // touch, and iced's widgets only listen for the second two.
        self.move_cursor(point);
        self.events
            .push(Event::Mouse(mouse::Event::CursorMoved { position: point }));
        self.events.push(Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        )));
    }

    /// A finger moved while touching the panel.
    pub fn touch_move(&mut self, point: Point) {
        // The controller keeps reporting a resting finger, and every one of those would be a
        // widget event. Same one-pixel threshold the previous backend used.
        let moved = self
            .cursor
            .position()
            .is_none_or(|last| (last.x - point.x).abs() > 1.0 || (last.y - point.y).abs() > 1.0);

        self.move_cursor(point);

        if moved {
            self.events
                .push(Event::Mouse(mouse::Event::CursorMoved { position: point }));
        }
    }

    /// The finger left the panel. The pointer stays where it was, because the widget that
    /// handles the release is the one under it.
    pub fn touch_up(&mut self) {
        self.events.push(Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        )));
    }

    fn move_cursor(&mut self, point: Point) {
        if self.cursor != mouse::Cursor::Available(point) {
            self.cursor = mouse::Cursor::Available(point);
        }
    }

    /// Runs this frame's events through `view` and draws it, and returns whether the tree asked
    /// for another frame.
    ///
    /// The events are delivered up to twice: the frame's own, and then the redraw request they
    /// asked for, which is what makes a press visible in the frame that saw it rather than in one
    /// the loop might never be asked to draw. A widget that asks *again* is asking to be drawn on
    /// the next frame, which is what the return value carries out of here.
    pub fn draw<R, T, M>(
        &mut self,
        renderer: &mut R,
        view: Element<'_, M, T, R>,
        messages: &mut Vec<M>,
        theme: &T,
        bounds: Size,
        text_color: Color,
    ) -> bool
    where
        R: iced_core::renderer::Renderer,
        T: theme::Base,
        M: Send + 'static,
    {
        let mut ui = UserInterface::build(
            view,
            bounds,
            std::mem::replace(&mut self.cache, Cache::new()),
            renderer,
        );

        let mut redraw_requested = false;

        for pass in 0..2 {
            self.events.push(Event::Window(
                window::Event::RedrawRequested(Instant::now()),
            ));

            let (state, _) = ui.update(
                &self.events,
                self.cursor,
                renderer,
                &mut self.clipboard,
                messages,
            );

            self.events.clear();

            redraw_requested = match state {
                State::Outdated => true,
                State::Updated { redraw_request, .. } => {
                    matches!(redraw_request, window::RedrawRequest::NextFrame)
                }
            };

            if !redraw_requested || pass == 1 {
                break;
            }
        }

        ui.draw(
            renderer,
            theme,
            &renderer::Style { text_color },
            self.cursor,
        );

        self.cache = ui.into_cache();

        redraw_requested
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: App> Application<A> {
    /// Takes ownership of `app` and allocates the buffers for a `width * height` panel.
    ///
    /// `None` if the buffers cannot be allocated.
    pub fn new(app: A, width: u32, height: u32) -> Option<Self> {
        // A program that installs no font of its own still has to draw text, and this is where
        // the host is entered: the default is a no-op if `boot` already installed one.
        crate::fonts::install_default();

        Some(Self {
            app,
            // iced's own default: the family the theme asks for and 16 logical pixels.
            renderer: Renderer::new(Font::default(), Pixels(16.0)),
            surface: Surface::new(width, height)?,
            tree: Tree::new(),
            messages: Vec::new(),
            dirty: true,
            redraw_requested: false,
        })
    }

    /// The screen, in logical pixels.
    pub fn bounds(&self) -> Size {
        self.surface.viewport().logical_size()
    }

    /// The RGB565 frame buffer, ready to be handed to the panel.
    pub fn panel(&self) -> &Pixmap565 {
        self.surface.panel()
    }

    /// Queues an event. For tests and for backends with more inputs than a touchscreen.
    pub fn push_event(&mut self, event: Event) {
        self.tree.push_event(event);
    }

    /// A finger touched the panel at `point`.
    pub fn touch_down(&mut self, point: Point) {
        self.tree.touch_down(point);
    }

    /// A finger moved while touching the panel.
    pub fn touch_move(&mut self, point: Point) {
        self.tree.touch_move(point);
    }

    /// The finger left the panel. The pointer stays where it was, because the widget that
    /// handles the release is the one under it.
    pub fn touch_up(&mut self) {
        self.tree.touch_up();
    }

    /// Runs one frame and returns the rectangles that changed, in physical pixels.
    ///
    /// An empty result means nothing moved and the panel does not need to be touched at all —
    /// which is the normal outcome of calling this on an idle screen.
    pub fn frame(&mut self) -> Vec<Rectangle> {
        // Work that is not the widget tree comes first: a `Program`'s tasks and subscriptions.
        // What they bring is a message like any other, and it has to be applied even on a frame
        // that would otherwise be skipped -- a panel with a task waiting on it is not an idle
        // panel.
        self.messages.extend(self.app.poll());

        if !self.dirty
            && !self.redraw_requested
            && self.tree.is_idle()
            && self.messages.is_empty()
            && !self.app.is_animating()
        {
            return Vec::new();
        }

        if self.app.is_animating() {
            self.app.tick();
        }

        let theme = self.app.theme();
        // `base()` and not `palette()`: the palette is `Option` (a theme may be a custom
        // catalog with no single palette) and exists for devtools. The base style is the
        // background and the default text color, which is what an app is framed by.
        let base = theme.base();
        let bounds = self.bounds();

        // Everything between "an event arrived" and "the tree is drawn" is the platform's half of
        // iced's contract, and it is the same for an `App` and for a `Program`: `Tree` is where it
        // lives, and this call is what both loops make.
        self.redraw_requested = self.tree.draw(
            &mut self.renderer,
            self.app.view(),
            &mut self.messages,
            &theme,
            bounds,
            base.text_color,
        );

        // One call, two implementations: `tiny-skia`'s surface unions the damage into a single
        // rectangle for its own reasons, and the recorded one replays each rectangle as it is.
        let damaged = self
            .surface
            .present(&mut self.renderer, base.background_color);

        self.dirty = !self.messages.is_empty();

        for message in self.messages.drain(..) {
            self.app.update(message);
        }

        // An update can produce work that finishes without waiting -- `Task::done` is the common
        // one -- and iced's own loop applies that in the frame that asked for it instead of
        // leaving it for the next one. One more round: whatever comes out of it needs a frame to
        // be drawn in, which is what `dirty` is for.
        for message in self.app.poll() {
            self.dirty = true;
            self.app.update(message);
        }

        damaged
    }

    /// The app, for a host that needs to read it — a status it displays, a test asserting what a
    /// tap did. Mutating goes through [`Application::update_app`], which schedules the frame that
    /// the change would otherwise not get.
    pub fn app(&self) -> &A {
        &self.app
    }

    /// Mutates the app from outside the loop — a clock the platform owns, a battery reading —
    /// and marks the screen for redrawing, because nothing else would know it changed.
    pub fn update_app(&mut self, update: impl FnOnce(&mut A)) {
        update(&mut self.app);
        self.dirty = true;
    }

    /// The default text color of a theme, for a backend that needs to paint outside the tree.
    pub fn text_color(&self) -> Color {
        self.app.theme().base().text_color
    }
}
