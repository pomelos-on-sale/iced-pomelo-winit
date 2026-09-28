//! The errors `iced_winit` is allowed to fail with, which is what `iced::Error` maps.
//!
//! The three variants are the facade's: `iced/src/error.rs` matches on exactly these, and it boxes
//! the window one (`WindowCreationFailed(error) => WindowCreationFailed(Box::new(error))`), so the
//! payload has to implement `std::error::Error` rather than be a `String`.
//!
//! Two of them cannot happen here and are declared anyway, because the facade's `From`
//! implementation names them: creating an executor is [`crate::Pump::new`], which is a
//! `Default`, and a graphics context is not a thing this platform has. The third is real, and it is
//! what `run` returns when the firmware never registered a board.

use std::fmt;

/// `run` needs a panel and a touch controller, and neither was registered.
#[derive(Debug)]
pub struct NoWindow;

impl fmt::Display for NoWindow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("no board was registered: call `set_board` before `run`")
    }
}

impl std::error::Error for NoWindow {}

/// The errors `run` can fail with.
#[derive(Debug)]
pub enum Error {
    /// The futures executor could not be created.
    ///
    /// Never returned here: the executor is a queue, and creating it cannot fail. The variant
    /// exists because `iced::Error` maps it.
    ExecutorCreationFailed(iced_futures::futures::io::Error),

    /// The application window could not be created.
    ///
    /// On this board: no board was registered, so there is nothing to draw on.
    WindowCreationFailed(NoWindow),

    /// The application graphics context could not be created.
    ///
    /// On this board: the panel's buffers could not be allocated.
    GraphicsCreationFailed(iced_graphics::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExecutorCreationFailed(error) => {
                write!(
                    formatter,
                    "the futures executor could not be created: {error}"
                )
            }
            Self::WindowCreationFailed(error) => write!(formatter, "{error}"),
            Self::GraphicsCreationFailed(error) => {
                write!(
                    formatter,
                    "the graphics context could not be created: {error}"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<iced_graphics::Error> for Error {
    fn from(error: iced_graphics::Error) -> Self {
        Self::GraphicsCreationFailed(error)
    }
}
