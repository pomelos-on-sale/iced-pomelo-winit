//! The board the loop runs on, and the loop itself.
//!
//! This crate is board-agnostic on purpose: it takes a `Point` and returns `Rectangle`s, and
//! the firmware's board (`pomelo-hal-esp32`) is the only place the ESP32-S3 appears. But iced's
//! `run(program)` takes one
//! argument and no hardware, so the board has to reach the loop some other way — the firmware
//! registers it with [`set_board`], and [`crate::run`] takes it back. That is the single piece of
//! global state in this stack, and it is the price of the entry point iced offers.
//!
//! It is not the only thing the board is needed *for*, though. The loop is generic over the
//! program, and a program names its renderer only as a type parameter — which means the loop
//! cannot build a renderer or a surface, and does not: it asks the compositor that `P::Renderer`
//! reports (`<P::Renderer as compositor::Default>::Compositor`) for both, and that compositor
//! presents. What the compositor needs from the board is somewhere for the pixels to go, and
//! [`Host::new`] gives it one by registering the board's panel with `iced_pomelo_gfx::panel::set`.
//! So the board is registered twice, in two places, with one meaning each: the loop reads the
//! touch, the compositor writes the pixels.
//!
//! [`Host`] is the loop that uses it, and it is a separate type from `run` so that a test can
//! drive it without any globals at all: hand it a board that answers touch by itself and stops
//! after a few frames.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use iced_core::theme::Base;
use iced_core::{Point, Rectangle, Size};
use iced_graphics::compositor::Compositor as _;
use iced_graphics::{compositor, Shell, Viewport};
use iced_program::Program;

use crate::application::Tree;
use crate::{Error, ProgramApp};

/// What the loop needs from the hardware.
pub trait Board {
    /// The size of the panel, in physical pixels.
    fn size(&self) -> (u32, u32);

    /// Where the finger is, or `None` when it is not touching the panel.
    ///
    /// Polled once per iteration, exactly as the firmware's own loop polls its controller: a
    /// touchscreen reports a resting finger continuously, and the edges are what matter.
    fn touch(&self) -> Option<Point>;

    /// Hands the damaged rectangles to the panel.
    fn flush(&mut self, damage: &[Rectangle], panel: &[u16]);

    /// Called when a frame drew nothing, which is where a real board waits.
    ///
    /// Without this the loop would spin at full speed on an idle screen; with it, the panel's
    /// power story is the board's business and not the loop's.
    fn idle(&mut self) {}

    /// Whether the loop should keep going.
    ///
    /// A real board always says `true`, and this is where a test says "that is enough".
    fn running(&self) -> bool {
        true
    }
}

impl Board for Box<dyn Board> {
    fn size(&self) -> (u32, u32) {
        (**self).size()
    }

    fn touch(&self) -> Option<Point> {
        (**self).touch()
    }

    fn flush(&mut self, damage: &[Rectangle], panel: &[u16]) {
        (**self).flush(damage, panel)
    }

    fn idle(&mut self) {
        (**self).idle();
    }

    fn running(&self) -> bool {
        (**self).running()
    }
}

/// The board, shared: the loop reads the touch through it, and the compositor presents through it.
///
/// One `Rc` rather than two registrations because it is one object — the firmware has one board —
/// and because the panel registration outlives the call that installs it: the compositor holds the
/// closure until it is dropped.
type BoardHandle = Rc<RefCell<Box<dyn Board>>>;

thread_local! {
    /// The board the firmware registered, until `run` takes it.
    static BOARD: RefCell<Option<Box<dyn Board>>> = const { RefCell::new(None) };
}

/// Hands the platform its board. Call this before [`crate::run`].
pub fn set_board(board: impl Board + 'static) {
    BOARD.with(|slot| *slot.borrow_mut() = Some(Box::new(board)));
}

/// Takes the registered board, leaving the slot empty.
pub(crate) fn take_board() -> Option<Box<dyn Board>> {
    BOARD.with(|slot| slot.borrow_mut().take())
}

/// The loop: the touch in, the frame, the pixels out.
///
/// Generic over the program and nothing else. The renderer, the surface and the presentation all
/// come from `<P::Renderer as compositor::Default>::Compositor`, because a program's renderer is a
/// type parameter and this loop is not allowed to name it — that is what lets iced's facade, whose
/// own wrapper is equally generic, hand its program to `run`.
pub struct Host<P>
where
    P: Program,
{
    program: ProgramApp<P>,
    tree: Tree,
    messages: Vec<P::Message>,
    /// Whether a message arrived since the last frame, so that its effect gets drawn.
    dirty: bool,
    /// Whether the tree asked for another frame.
    redraw_requested: bool,
    compositor: <P::Renderer as compositor::Default>::Compositor,
    renderer: P::Renderer,
    surface: <<P::Renderer as compositor::Default>::Compositor as compositor::Compositor>::Surface,
    viewport: Viewport,
    board: BoardHandle,
    /// Set by the panel registration when the compositor presents, read once per iteration.
    flushed: Rc<Cell<bool>>,
    /// Whether a finger was down at the last iteration, so that the edges can be found.
    touching: bool,
}

impl<P> Host<P>
where
    P: Program + 'static,
{
    /// Registers `board`, boots `program` onto it, and asks the program's compositor for a
    /// renderer and a panel-sized surface.
    ///
    /// The fonts the program asks for in its settings are loaded here, which is iced's own
    /// arrangement: the settings travel with the program, and whoever creates the compositor
    /// installs them.
    pub fn new(program: P, board: Box<dyn Board>) -> Result<Self, Error> {
        // Text needs a font, and a program that installs none is the normal case for something
        // written for iced: the default fills that gap unless the program's authors already did.
        crate::fonts::install_default();

        let (width, height) = board.size();
        let board: BoardHandle = Rc::new(RefCell::new(board));

        // The panel, which is this board's display connection: iced's compositor is created with
        // one, and there is no window handle to give it.
        let flushed = Rc::new(Cell::new(false));
        iced_pomelo_gfx::panel::set({
            let board = Rc::clone(&board);
            let flushed = Rc::clone(&flushed);

            move |pixels, damage| {
                flushed.set(true);
                board.borrow_mut().flush(damage, pixels);
            }
        });

        let settings = program.settings();
        let graphics: iced_graphics::Settings = settings.clone().into();

        let mut compositor = block_on(
            <<P::Renderer as compositor::Default>::Compositor as compositor::Compositor>::new(
                graphics,
                iced_pomelo_gfx::Panel,
                iced_pomelo_gfx::Panel,
                Shell::headless(),
            ),
        )
        .map_err(Error::GraphicsCreationFailed)?;

        for font in settings.fonts {
            compositor.load_font(font);
        }

        let renderer = compositor.create_renderer();
        let surface = compositor.create_surface(iced_pomelo_gfx::Panel, width, height);

        Ok(Self {
            program: ProgramApp::new(program),
            tree: Tree::new(),
            messages: Vec::new(),
            dirty: true,
            redraw_requested: false,
            compositor,
            renderer,
            surface,
            viewport: Viewport::with_physical_size(Size::new(width, height), 1.0),
            board,
            flushed,
            touching: false,
        })
    }

    /// The program, for a host that wants to read it.
    pub fn app(&self) -> &ProgramApp<P> {
        &self.program
    }

    /// One iteration: the touch, the frame, and the panel if anything changed.
    ///
    /// Returns whether anything was painted, which is what the firmware's log counts.
    pub fn step(&mut self) -> bool {
        // The same edge detection the firmware does by hand, so that a touch that is resting is
        // not re-delivered: `touch_move` drops sub-pixel movement, but a press that repeated every
        // iteration would be a stream of presses.
        match (self.board.borrow().touch(), self.touching) {
            (Some(point), false) => {
                self.touching = true;
                self.tree.touch_down(point);
            }
            (Some(point), true) => self.tree.touch_move(point),
            (None, true) => {
                self.touching = false;
                self.tree.touch_up();
            }
            (None, false) => {}
        }

        // The compositor presents *inside* the frame, so the loop learns whether the panel was
        // touched from the registration it made rather than from a return value: iced's
        // `Compositor::present` reports a surface error and no damage.
        self.flushed.set(false);
        self.frame();
        let painted = self.flushed.get();

        if !painted {
            self.board.borrow_mut().idle();
        }

        painted
    }

    /// Runs until the board stops answering.
    pub fn run(&mut self) -> Result<(), Error> {
        while self.board.borrow().running() {
            self.step();
        }

        Ok(())
    }

    /// One frame: what the program has to say, the events, the draw, and the panel.
    fn frame(&mut self) {
        self.messages.extend(self.program.poll());

        if !self.dirty
            && !self.redraw_requested
            && self.tree.is_idle()
            && self.messages.is_empty()
            && !self.program.is_working()
        {
            return;
        }

        let theme = self.program.theme();
        // `base()` and not `palette()`: the palette is `Option` (a theme may be a custom catalog
        // with no single palette) and exists for devtools. The base style is the background and
        // the default text color, which is what an app is framed by.
        let base = theme.base();
        let bounds = self.viewport.logical_size();

        self.redraw_requested = self.tree.draw(
            &mut self.renderer,
            self.program.view(),
            &mut self.messages,
            &theme,
            bounds,
            base.text_color,
        );

        // Presenting is the compositor's, and a failure is a surface error with nowhere to go: this
        // panel does not go away. The frame is simply not presented. (Our compositor never fails;
        // the `Result` is iced's contract.)
        let _ = self.compositor.present(
            &mut self.renderer,
            &mut self.surface,
            &self.viewport,
            base.background_color,
            || {},
        );

        self.dirty = !self.messages.is_empty();

        for message in self.messages.drain(..) {
            self.program.update(message);
        }

        // An update can produce work that finishes without waiting -- `Task::done` is the common
        // one -- and iced's own loop applies that in the frame that asked for it instead of
        // leaving it for the next one. One more round: whatever comes out of it needs a frame to
        // be drawn in, which is what `dirty` is for.
        for message in self.program.poll() {
            self.dirty = true;
            self.program.update(message);
        }
    }
}

/// Polls a future to completion.
///
/// The future iced's compositor is created through is ready on its first poll — there is no I/O
/// behind a panel — so this is a loop and not a runtime. `Waker::noop` is what keeps it four lines.
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let mut context = Context::from_waker(Waker::noop());

    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}
