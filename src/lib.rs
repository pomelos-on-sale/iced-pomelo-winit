//! The platform iced's facade expects, under the name it expects.
//!
//! This crate is called `iced_winit` for one reason: `[patch.crates-io]` matches by **package
//! name**. The `iced` facade depends on `iced_winit` unconditionally — it is not an optional
//! feature — and the real one needs winit, which does not build for ESP-IDF. So the only way an
//! app can write `use iced::...` on this board is for something else to answer to that name, and
//! what answers is `pomelo-iced-host`: the event loop, the panel's buffers, the fonts, and (with
//! the `renderer` feature) the ability to run an iced `Program`.
//!
//! The facade asks for very little of it:
//!
//! | it wants | here |
//! | --- | --- |
//! | `run(program)` | [`run`], over [`Host`] and a [`Board`] the firmware registered |
//! | `Error` | [`Error`], with the three variants `iced::Error` maps |
//! | `core`, `graphics`, `program`, `runtime` | re-exports of the same crates, because the facade reaches iced through `iced_winit` |
//!
//! What it does *not* ask for is the rest of `iced_winit`: `Settings`, the window settings a
//! `Program` can state, `Clipboard`, `Proxy`, `conversion`, the `window` module. There is no
//! window here for any of them to describe, and not writing them is the point — see
//! `pomelo-iced-host`'s crate docs for why that layer is shaped the way it is.
//!
//! # What is still missing for the facade to actually work
//!
//! `iced_renderer::Renderer` has to *be* this stack's renderer. An app writes
//! `type Renderer = iced::Renderer`, and the widget tree it hands back must be the one
//! [`run`] can draw; today `iced_renderer`'s `custom` feature points that name at
//! `iced_tiny_skia::Renderer` instead. That is one more change to the fork — a `pomelo` feature
//! naming `iced-pomelo-gfx`, the way `tiny-skia` names `iced_tiny_skia` — and until it lands,
//! patching this crate in gets you the signature and not yet the build.

pub use iced_core as core;
pub use iced_futures as futures;
pub use iced_graphics as graphics;
pub use iced_program as program;
pub use iced_runtime as runtime;

pub mod error;

pub use error::Error;

#[cfg(feature = "renderer")]
pub mod board;

#[cfg(feature = "renderer")]
pub use board::{set_board, Board, Host};

/// Runs `program` until the board says to stop.
///
/// This is the call `iced::application(..).run()` makes, and its signature is the facade's: one
/// argument, and no place to put the hardware. The board comes from [`set_board`], which the
/// firmware calls before handing over — the one piece of global state in this stack, and it
/// exists because iced's signature has no room for it.
#[cfg(feature = "renderer")]
pub fn run<P>(program: P) -> Result<(), Error>
where
    P: program::Program<Renderer = pomelo_iced_host::Renderer> + 'static,
{
    let board = board::take_board().ok_or(Error::WindowCreationFailed(error::NoWindow))?;

    Host::new(program, board)?.run()
}
