//! The part of a frame that is neither the app's nor the renderer's: the widget tree's cache, the
//! events waiting for it, the pointer, and the two rendering types the loop draws with.
//!
//! Both entry points share it: [`crate::run`], hosting an iced
//! [`Program`](crate::program::Program), and this crate's own tests, driving the same tree by
//! hand. The loop itself is [`crate::Host`], which owns a `Tree` and the hardware
//! ([`crate::Board`]).

use iced_core::widget::{operation::Outcome, Operation};
use iced_core::{
    mouse, renderer, theme, touch, window, Color, Element, Event, Point, Size,
};
use iced_runtime::user_interface::{Cache, State, UserInterface};
use std::time::Instant;

/// The renderer our apps are written against.
///
/// A program names this exactly once, in the `Element` its
/// [`view`](crate::program::Program::view) returns. It is `iced-pomelo-gfx`'s renderer when the
/// recorded path is on, and `iced_tiny_skia`'s otherwise -- the two are interchangeable from the
/// app's side, which is what makes the switch a switch.
#[cfg(feature = "renderer")]
pub type Renderer = iced_pomelo_gfx::Renderer;

#[cfg(not(feature = "renderer"))]
pub type Renderer = iced_tiny_skia::Renderer;

/// The widget tree's side of a frame.
///
/// The events a platform delivers, the cursor, the clipboard, the tree's cache, and the platform's
/// half of iced's redraw contract. That last one is the subtle part: iced's widgets decide what
/// they look like when they see a redraw request (`Button::draw` paints `self.status`, and
/// `self.status` is written in exactly one place — the `window::Event::RedrawRequested` arm), and
/// there is no window here to send one.
///
/// What is *not* in here is who owns the renderer or what a message means: that is the loop's, and
/// the loop is [`crate::Host`].
pub struct Tree {
    cache: Cache,
    cursor: mouse::Cursor,
    clipboard: crate::clipboard::PomeloClipboard,
    events: Vec<Event>,
    /// Widget operations waiting for the next [`Tree::draw`] to apply them. They need the
    /// `UserInterface`, which only exists inside that call -- see [`Tree::queue_operations`].
    operations: Vec<Box<dyn Operation>>,
    next_redraw: Option<Instant>,
}

impl Tree {
    pub fn new() -> Self {
        Self {
            cache: Cache::new(),
            cursor: mouse::Cursor::Unavailable,
            clipboard: crate::clipboard::PomeloClipboard,
            events: Vec::new(),
            operations: Vec::new(),
            next_redraw: None,
        }
    }

    /// Earliest time when a widget requested a scheduled redraw, if any.
    pub fn next_redraw(&self) -> Option<Instant> {
        self.next_redraw
    }

    /// Whether an event is waiting to be delivered.
    pub fn is_idle(&self) -> bool {
        self.events.is_empty()
    }

    /// Queues an event. For tests and for backends with more inputs than a touchscreen.
    pub fn push_event(&mut self, event: Event) {
        self.events.push(event);
    }

    /// Queues widget operations for the next [`Tree::draw`] to apply to the tree.
    ///
    /// A widget operation reaches into a `UserInterface` -- it carries widget state, not messages
    /// -- and the interface is built and dropped every time `draw` runs. So an operation has to
    /// wait for it, and this is where it waits. [`crate::ProgramApp::take_operations`] is what
    /// fills it.
    pub fn queue_operations(&mut self, operations: Vec<Box<dyn Operation>>) {
        self.operations.extend(operations);
    }

    /// Whether a widget operation is waiting to be applied.
    ///
    /// A frame with one pending is not a frame to skip: the operation is the work, and nothing
    /// else may ask for the draw that would apply it.
    pub fn has_operations(&self) -> bool {
        !self.operations.is_empty()
    }

    /// A finger touched the panel at `point`.
    pub fn touch_down(&mut self, point: Point) {
        self.move_cursor(point);
        self.events.push(Event::Touch(touch::Event::FingerPressed {
            id: touch::Finger(0),
            position: point,
        }));
    }

    /// A finger moved while touching the panel.
    pub fn touch_move(&mut self, point: Point) {
        // Discard identical coordinates from a resting finger, while allowing
        // continuous 1-pixel incremental movement during slow dragging.
        let moved = self
            .cursor
            .position()
            .is_none_or(|last| (last.x - point.x).abs() >= 1.0 || (last.y - point.y).abs() >= 1.0);

        if moved {
            self.move_cursor(point);
            self.events.push(Event::Touch(touch::Event::FingerMoved {
                id: touch::Finger(0),
                position: point,
            }));
        }
    }

    /// The finger left the panel. The pointer stays where it was, because the widget that
    /// handles the release is the one under it.
    pub fn touch_up(&mut self) {
        let position = self.cursor.position().unwrap_or(Point::ORIGIN);
        self.events.push(Event::Touch(touch::Event::FingerLifted {
            id: touch::Finger(0),
            position,
        }));
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
        // The three phases of the tree are timed through the recorded renderer's counters, which
        // this crate only depends on with `profile` on: that feature implies `renderer`, because a
        // split of a frame drawn by something else is not a split of anything.
        #[cfg(feature = "profile")]
        let build = iced_pomelo_gfx::profile::start(iced_pomelo_gfx::profile::Phase::TreeBuild);

        let mut ui = UserInterface::build(
            view,
            bounds,
            std::mem::replace(&mut self.cache, Cache::new()),
            renderer,
        );

        #[cfg(feature = "profile")]
        drop(build);

        #[cfg(feature = "profile")]
        let update = iced_pomelo_gfx::profile::start(iced_pomelo_gfx::profile::Phase::TreeUpdate);

        self.next_redraw = None;
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

            match state {
                State::Outdated => {
                    redraw_requested = true;
                    self.next_redraw = None;
                }
                State::Updated { redraw_request, .. } => match redraw_request {
                    window::RedrawRequest::NextFrame => {
                        redraw_requested = true;
                        self.next_redraw = None;
                    }
                    window::RedrawRequest::At(at) => {
                        if Instant::now() >= at {
                            redraw_requested = true;
                            self.next_redraw = None;
                        } else {
                            self.next_redraw = Some(self.next_redraw.map_or(at, |prev| prev.min(at)));
                        }
                    }
                    window::RedrawRequest::Wait => {}
                },
            };

            if !redraw_requested || pass == 1 {
                break;
            }
        }

        // A widget operation holds widget state, not messages, so the only thing it can be
        // applied to is the `UserInterface` -- built just above and dropped at the end of this
        // call. That makes this the one place it can run. Applied after the frame's events, so
        // the frame that asked for it shows its effect.
        let had_operations = !self.operations.is_empty();
        for operation in std::mem::take(&mut self.operations) {
            let mut pending = Some(operation);

            while let Some(mut current) = pending.take() {
                ui.operate(renderer, current.as_mut());

                match current.finish() {
                    Outcome::None | Outcome::Some(()) => {}
                    Outcome::Chain(next) => pending = Some(next),
                }
            }
        }

        // If widget operations ran (such as focusing a text input), inform the widgets of the
        // impending redraw so newly focused widgets can register their scheduled redraw requests
        // (like cursor blinking).
        if had_operations {
            let (state, _) = ui.update(
                &[Event::Window(window::Event::RedrawRequested(Instant::now()))],
                self.cursor,
                renderer,
                &mut self.clipboard,
                messages,
            );

            match state {
                State::Outdated => {
                    redraw_requested = true;
                    self.next_redraw = None;
                }
                State::Updated { redraw_request, .. } => match redraw_request {
                    window::RedrawRequest::NextFrame => {
                        redraw_requested = true;
                        self.next_redraw = None;
                    }
                    window::RedrawRequest::At(at) => {
                        if Instant::now() >= at {
                            redraw_requested = true;
                            self.next_redraw = None;
                        } else {
                            self.next_redraw = Some(self.next_redraw.map_or(at, |prev| prev.min(at)));
                        }
                    }
                    window::RedrawRequest::Wait => {}
                },
            };
        }

        #[cfg(feature = "profile")]
        drop(update);

        #[cfg(feature = "profile")]
        let record = iced_pomelo_gfx::profile::start(iced_pomelo_gfx::profile::Phase::TreeRecord);

        ui.draw(
            renderer,
            theme,
            &renderer::Style { text_color },
            self.cursor,
        );

        #[cfg(feature = "profile")]
        drop(record);

        self.cache = ui.into_cache();

        redraw_requested
    }
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::touch;

    #[test]
    fn touch_generates_single_events() {
        let mut tree = Tree::new();
        let pt = Point::new(100.0, 100.0);

        tree.touch_down(pt);
        assert_eq!(tree.events.len(), 1);
        assert!(matches!(
            tree.events[0],
            Event::Touch(touch::Event::FingerPressed { position, .. }) if position == pt
        ));
        assert_eq!(tree.cursor, mouse::Cursor::Available(pt));

        tree.events.clear();
        let pt2 = Point::new(120.0, 120.0);
        tree.touch_move(pt2);
        assert_eq!(tree.events.len(), 1);
        assert!(matches!(
            tree.events[0],
            Event::Touch(touch::Event::FingerMoved { position, .. }) if position == pt2
        ));
        assert_eq!(tree.cursor, mouse::Cursor::Available(pt2));

        tree.events.clear();
        tree.touch_up();
        assert_eq!(tree.events.len(), 1);
        assert!(matches!(
            tree.events[0],
            Event::Touch(touch::Event::FingerLifted { position, .. }) if position == pt2
        ));
    }

    #[test]
    fn pick_list_opens_on_touch_down() {
        use iced_widget::pick_list;

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum OptionItem {
            A,
            B,
        }
        impl std::fmt::Display for OptionItem {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{:?}", self)
            }
        }

        let mut tree = Tree::new();
        let options = [OptionItem::A, OptionItem::B];
        let selected = Some(OptionItem::A);

        let mut renderer = iced_tiny_skia::Renderer::new(
            iced_core::Font::DEFAULT,
            iced_core::Pixels(16.0),
        );
        let bounds = Size::new(480.0, 480.0);

        // Frame 1: initial layout
        let view: Element<'_, OptionItem, iced_core::Theme, iced_tiny_skia::Renderer> =
            Element::new(pick_list(&options[..], selected, |opt| opt).on_open(OptionItem::B));
        let mut messages = Vec::new();
        tree.draw(
            &mut renderer,
            view,
            &mut messages,
            &iced_core::Theme::Dark,
            bounds,
            Color::WHITE,
        );

        // Touch on the pick_list widget at (10, 10)
        tree.touch_down(Point::new(10.0, 10.0));

        // Frame 2: process touch_down
        let view: Element<'_, OptionItem, iced_core::Theme, iced_tiny_skia::Renderer> =
            Element::new(pick_list(&options[..], selected, |opt| opt).on_open(OptionItem::B));
        let mut messages = Vec::new();
        tree.draw(
            &mut renderer,
            view,
            &mut messages,
            &iced_core::Theme::Dark,
            bounds,
            Color::WHITE,
        );

        assert_eq!(messages, vec![OptionItem::B], "pick_list should publish on_open message when opened by touch");

        // Frame 3: touch on option B in the overlay menu
        tree.touch_down(Point::new(10.0, 70.0));
        let view: Element<'_, OptionItem, iced_core::Theme, iced_tiny_skia::Renderer> =
            Element::new(pick_list(&options[..], selected, |opt| opt).on_open(OptionItem::B));
        let mut messages = Vec::new();
        tree.draw(
            &mut renderer,
            view,
            &mut messages,
            &iced_core::Theme::Dark,
            bounds,
            Color::WHITE,
        );

        assert_eq!(messages, vec![OptionItem::B], "touching option should select it");
    }
}

