//! In-memory clipboard backend for Pomelo OS.
//!
//! Provides the embedded platform implementation of [`iced_core::clipboard::Clipboard`]
//! backed by a thread-safe global in-memory buffer shared across apps and widgets.

use std::sync::RwLock;
use iced_core::clipboard::{Clipboard, Kind};

static CLIPBOARD: RwLock<String> = RwLock::new(String::new());

/// Reads the current contents of the system clipboard.
pub fn read(_kind: Kind) -> Option<String> {
    let guard = CLIPBOARD.read().ok()?;
    if guard.is_empty() {
        None
    } else {
        Some(guard.clone())
    }
}

/// Writes text into the system clipboard.
pub fn write(_kind: Kind, contents: String) {
    if let Ok(mut guard) = CLIPBOARD.write() {
        *guard = contents;
    }
}

/// Clears the system clipboard.
pub fn clear() {
    if let Ok(mut guard) = CLIPBOARD.write() {
        guard.clear();
    }
}

/// A clipboard implementation backed by Pomelo OS's shared memory clipboard.
#[derive(Debug, Clone, Copy, Default)]
pub struct PomeloClipboard;

impl Clipboard for PomeloClipboard {
    fn read(&self, kind: Kind) -> Option<String> {
        read(kind)
    }

    fn write(&mut self, kind: Kind, contents: String) {
        write(kind, contents);
    }
}
