//! The Pomelo OS host for iced, under the name iced's facade asks for.
//!
//! # Why this crate is called `iced_winit`
//!
//! `[patch.crates-io]` matches by **package name**, and the `iced` facade depends on `iced_winit`
//! unconditionally — it is not an optional feature — while the real `iced_winit` needs winit,
//! which does not build for ESP-IDF. So the only way an app can write `use iced::...` on this
//! board is for something else to answer to that name, and what answers is this: the platform
//! layer, wearing the name the facade insists on.
//!
//! The facade asks for very little of it, and the rest of `iced_winit` is deliberately absent:
//!
//! | it wants | here |
//! | --- | --- |
//! | `run(program)` | [`run`], over [`Host`] and a [`Board`] the firmware registered |
//! | `Error` | [`Error`], with the three variants `iced::Error` maps |
//! | `core`, `graphics`, `program`, `runtime` | re-exports of the same crates, because on a desktop `iced::Element` is reached *through* `iced_winit` |
//!
//! What it does not ask for is `Settings`, the window settings a `Program` can state, `Clipboard`,
//! `Proxy`, `conversion` or the `window` module. There is no window here for any of them to
//! describe, and not writing them is the point.
//!
//! # What the crate actually is
//!
//! It occupies the place `iced_winit` occupies on a desktop, without a window. iced's own loop is
//! `iced_winit::run`, which needs winit and a window. Its shape is three phases owned by
//! `iced_runtime::UserInterface` — build the widget tree, feed it the events, draw it — plus two
//! that belong to the platform: where the events come from, and where the pixels go. On this board
//! those two are a touch controller and a 480×480 panel, so this crate provides them and nothing
//! else: the [`Board`] a firmware registers, the loop that drives it ([`Host`], or [`run`] when the
//! board is looked up instead of handed over), and [`Tree`] for the part of a frame that is neither
//! an app's nor a renderer's.
//!
//! ```text
//! iced_widget / iced_runtime      widgets and the runtime
//!         ↓   the renderer contract — `iced-pomelo-gfx`
//! iced-pomelo-gfx                 records the frame, replays it into RGB565, presents it
//!         ↓   RGB565, plus the damaged rectangles
//! iced_winit (this crate)          the events, the tree, the fonts, and the loop
//!         ↓   RGB565
//! the panel
//! ```
//!
//! # The two shapes a program can have here
//!
//! * An iced [`program::Program`](iced_program::Program) — what `iced::run` and every desktop app
//!   already is — through [`Host`] or [`run`]. The loop is generic over it, and the renderer, the
//!   surface and the presentation come from the compositor the program's renderer reports. This is
//!   the shape the facade's `iced::application(..).run()` lands in.
//! * This crate's own [`App`] — no state split, no tasks, no window id — through [`Application`],
//!   which the seven apps in this OS are written against. It names its renderer concretely, which
//!   is why it is not the shape `run` takes; the work of moving those apps onto `Program` is what
//!   would let this one go.
//!
//! # The two paths
//!
//! How a frame becomes pixels is chosen at compile time:
//!
//! * without the `renderer` feature, `iced_tiny_skia`'s engine rasterises into a
//!   `width * height * 4` buffer, the damaged rectangles are converted into RGB565, and this crate's
//!   `surface` module owns all three buffers;
//! * with it, `iced-pomelo-gfx` records the frame's commands, replays them into the panel's RGB565
//!   buffer through [`Surface`], and presents the damaged regions — the `panel` registration in
//!   [`Host::new`] is where those pixels reach the board.
//!
//! `tiny-skia` is the default until the recorded path passes the frame-cost work written up in
//! `issues-and-todo/260928-01-iced-canvas-animation-cost.md`.
//!
//! # The buffers
//!
//! We own them, so their layout is ours to state rather than something to inherit.
//!
//! * The `tiny-skia` path's draw target is `width * height * 4` bytes of premultiplied 8888.
//!   `PixmapMut::from_bytes` asks for R, G, B, A, but `iced_tiny_skia` writes **B, G, R, A**: the
//!   renderer is written for softbuffer's `0x00RRGGBB` frame buffer, which iced hands to
//!   `PixmapMut` as raw bytes, so *every* colour is pre-swapped on the way in — `into_color` and
//!   `into_paint` are `from_rgba(b, g, r, a)`, and glyph packing and image decoding are the same.
//!   The swap is uniform across all of them, so a uniform inversion is exact; `Surface::convert`
//!   just reads the bytes in the order they were written. Alpha is written and premultiplied too,
//!   but neither softbuffer nor RGB565 keeps it.
//! * The panel's frame buffer is RGB565, and only the damaged rectangles are written into it.
//!
//! # Where `pomelo-gfx` comes in
//!
//! With the `renderer` feature it is the rasteriser, and the switch is worth doing for measured
//! reasons rather than tidiness. It removes, in one change:
//!
//! * the 900 KiB RGBA surface and the 225 KiB clip mask, because the renderer draws straight into
//!   RGB565;
//! * the per-frame `convert`, and with it the byte-order inversion above;
//! * `PixmapMut::draw_pixmap` — the blit every glyph and every image goes through, measured at
//!   **4.44 µs/px, 86% of paint** on this board in the earlier probe.
//!
//! # What a program written for iced needs
//!
//! `iced::run(update, view)` — or `iced::application(..).run()` — ends in
//! `iced_winit::run(program)`, which is this crate's [`run`]. Two things have to be true for that
//! to build, and both are in this crate's features rather than in the app:
//!
//! * `iced_renderer`'s `pomelo` feature, so that `iced::Renderer` — the renderer an app names once,
//!   in its `Element` — *is* the one that draws it. That is turned on by this crate's `renderer`
//!   feature, which an app enables by depending on the host:
//!
//!   ```toml
//!   pomelo-iced-host = { package = "iced_winit", path = "...", features = ["renderer"] }
//!   ```
//!
//! * no renderer *bound* in this shell, which is the other half of the same problem: iced's facade
//!   hands over a wrapper (`iced::Application<P>`) that is generic over `P::Renderer`, so a shell
//!   that insisted on one renderer could never accept it. [`Host`] gets the renderer from
//!   `<P::Renderer as compositor::Default>::Compositor` instead, exactly as upstream's shell does.
//!
//! # Fonts
//!
//! A program written for iced installs no font — on a desktop the operating system has them — and
//! this board has none, so this crate carries one: a 16 KiB Latin subset of Roboto
//! (`fonts/README.md`), installed by both entry points unless an app or a firmware installed a
//! font of its own first. [`fonts::install`] always wins; the default only fills a gap. A program
//! can also bring fonts through iced's own channel, `Program::settings().fonts`.
//!
//! # What is still missing
//!
//! The gap is in the renderer and the widget set rather than here: `text_input` and `text_editor`
//! (the renderer's `fill_editor` is a `todo!()`), text drawn inside a `canvas`, images, and
//! `iced::window` — which a program with more than one window would need, and which this board has
//! no answer for. Widget operations that arrive as `Action::Widget` are still dropped; see
//! `hosting.rs`.

pub use iced_core as core;
pub use iced_futures as futures;
pub use iced_graphics as graphics;
pub use iced_program as program;
pub use iced_runtime as runtime;

pub mod error;

pub use error::Error;

pub mod application;
pub mod damage;
pub mod executor;
pub mod fonts;

// The recorded path's surface — the panel's frame buffer and the recorded damage — lives in
// `iced-pomelo-gfx`, beside the commands it replays; with the `renderer` feature this is a
// re-export of it, and without it the module below is the `tiny-skia` path's own.
#[cfg(feature = "renderer")]
pub use iced_pomelo_gfx::Surface;
#[cfg(not(feature = "renderer"))]
mod surface;
#[cfg(not(feature = "renderer"))]
pub use surface::Surface;

// Running an iced `Program`, which is the entry point an app written for iced already has. Behind
// the recorded renderer because `Program`'s renderer has to be a *headless* one that can name a
// compositor, and in this stack only `iced-pomelo-gfx` implements those.
//
// `hosting`, not `program`: the name `program` is taken by the re-export below, which is
// `iced_program` and has to answer to that name for the facade (`use iced_winit::program;`).
#[cfg(feature = "renderer")]
pub mod hosting;

#[cfg(feature = "renderer")]
pub mod board;

pub use application::{App, Application, Renderer, Tree};
pub use executor::Pump;

#[cfg(feature = "renderer")]
pub use board::{set_board, Board, Host};
#[cfg(feature = "renderer")]
pub use hosting::ProgramApp;

// Hosts speak these: a touchscreen hands over a `Point`, and a panel takes a `Rectangle` to flush.
// Re-exported so that firmware code does not have to depend on `iced_core` to talk to this layer.
pub use iced_core::{Point, Rectangle};

/// Runs `program` until the board says to stop.
///
/// This is the call `iced::application(..).run()` makes, and its signature is the facade's: one
/// argument, and no place to put the hardware. The board comes from [`set_board`], which the
/// firmware calls before handing over — the one piece of global state in this stack, and it
/// exists because iced's signature has no room for it.
///
/// Note what is *not* in the bounds: the renderer. A `Program` names its own renderer type, and
/// iced's facade hands its shell a program whose renderer is a *type parameter* — a wrapper generic
/// over `P::Renderer` — so a shell that insisted on one particular renderer could not accept it.
/// This one asks the program's compositor instead
/// (`<P::Renderer as compositor::Default>::Compositor`), which is where upstream's `run` gets it
/// too.
#[cfg(feature = "renderer")]
pub fn run<P>(program: P) -> Result<(), Error>
where
    P: program::Program + 'static,
{
    let board = board::take_board().ok_or(Error::WindowCreationFailed(error::NoWindow))?;

    Host::new(program, board)?.run()
}
