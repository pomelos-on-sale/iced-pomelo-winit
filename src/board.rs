//! The board the loop runs on, and the loop itself.
//!
//! `pomelo-iced-host` is board-agnostic on purpose: it takes a `Point` and returns `Rectangle`s,
//! and `board-bridge` is the only place the ESP32-S3 appears. But iced's `run(program)` takes one
//! argument and no hardware, so the board has to reach the loop some other way — the firmware
//! registers it with [`set_board`], and [`crate::run`] takes it back. That is the single piece of
//! global state in this stack, and it is the price of the entry point iced offers.
//!
//! [`Host`] is the loop that uses it, and it is a separate type from `run` so that a test can
//! drive it without any globals at all: hand it a board that answers touch by itself and stops
//! after a few frames.

use std::cell::RefCell;

use iced_core::{Point, Rectangle};
use iced_program::Program;
use pomelo_iced_host::{Application, ProgramApp, Renderer};

use crate::Error;

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
        (**self).flush(damage, panel);
    }

    fn idle(&mut self) {
        (**self).idle();
    }

    fn running(&self) -> bool {
        (**self).running()
    }
}

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
/// The renderer bound is `Program`'s, not ours: a `Program` names its own renderer type, and the
/// widget tree it returns has to be one this stack's [`Renderer`] can draw.
pub struct Host<P: Program<Renderer = Renderer>> {
    app: Application<ProgramApp<P>>,
    board: Box<dyn Board>,
    /// Whether a finger was down at the last iteration, so that the edges can be found.
    touching: bool,
}

impl<P> Host<P>
where
    P: Program<Renderer = Renderer>,
{
    /// Allocates the panel's buffers and boots `program` onto `board`.
    pub fn new(program: P, board: Box<dyn Board>) -> Result<Self, Error> {
        let (width, height) = board.size();

        let app = Application::new(ProgramApp::new(program), width, height).ok_or_else(|| {
            Error::GraphicsCreationFailed(iced_graphics::Error::BackendError(format!(
                "could not allocate the buffers for a {width}x{height} panel"
            )))
        })?;

        Ok(Self {
            app,
            board,
            touching: false,
        })
    }

    /// The program, for a host that wants to read it.
    pub fn app(&self) -> &ProgramApp<P> {
        self.app.app()
    }

    /// One iteration: the touch, the frame, and the panel if anything changed.
    ///
    /// Returns whether anything was painted, which is what the firmware's log counts.
    pub fn step(&mut self) -> bool {
        // The same edge detection the firmware does by hand, so that a touch that is resting is
        // not re-delivered: `touch_move` drops sub-pixel movement, but a press that repeated every
        // iteration would be a stream of presses.
        match (self.board.touch(), self.touching) {
            (Some(point), false) => {
                self.touching = true;
                self.app.touch_down(point);
            }
            (Some(point), true) => self.app.touch_move(point),
            (None, true) => {
                self.touching = false;
                self.app.touch_up();
            }
            (None, false) => {}
        }

        let damage = self.app.frame();

        if damage.is_empty() {
            self.board.idle();
        } else {
            self.board.flush(&damage, self.app.panel().data());
        }

        !damage.is_empty()
    }

    /// Runs until the board stops answering.
    pub fn run(&mut self) -> Result<(), Error> {
        while self.board.running() {
            self.step();
        }

        Ok(())
    }
}
