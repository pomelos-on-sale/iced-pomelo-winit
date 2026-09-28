//! Getting a font into iced, which has none on a device.
//!
//! `iced_graphics::text::font_system()` is a global `RwLock<FontSystem>`, and iced initialises it
//! with `FontSystem::new_with_fonts`, which *also* scans the operating system's font
//! directories. A desktop has those; ESP-IDF does not. What is left after the scan is whatever
//! iced embedded itself: `Iced-Icons.ttf`, an icon font, plus `FiraSans-Regular.ttf` if the
//! `fira-sans` feature is on — which it is not in our feature selection, and which would cost
//! 441 KiB of flash if it were.
//!
//! So we load our own. Two steps, and the second is the one that matters:
//!
//! 1. `FontSystem::load_font` puts the bytes in the database.
//! 2. `iced_core::Font::default()` is `Family::SansSerif` — not a name — so the face has to be
//!    made *the* sans-serif family, or the default would resolve to the icon font and every
//!    label would come out as glyphs of nothing.
//!
//! Step 2 asks the database which family it just indexed instead of taking a name from the
//! caller: a name that does not match byte for byte fails silently, and silently missing text is
//! exactly the failure this module exists to prevent.

use std::borrow::Cow;

/// Installs `bytes` as iced's default font.
///
/// Call this before the first frame. Calling it twice with the same bytes is free — iced keys
/// loaded fonts by address for borrowed slices, which static `include_bytes!` data is.
pub fn install(bytes: &'static [u8]) {
    let mut system = iced_graphics::text::font_system()
        .write()
        .expect("write font system");

    system.load_font(Cow::Borrowed(bytes));

    // The face we just added is the last one indexed.
    let family = system
        .raw()
        .db_mut()
        .faces()
        .last()
        .and_then(|face| face.families.first())
        .map(|(family, _)| family.clone());

    if let Some(family) = family {
        system.raw().db_mut().set_sans_serif_family(family);
    }
}

/// The names of the families currently in the database. For diagnostics — a device that renders
/// no text should be asked what it actually has.
pub fn families() -> Vec<String> {
    // A write guard, not a read one: iced only exposes `raw()` mutably.
    let mut system = iced_graphics::text::font_system()
        .write()
        .expect("write font system");

    system
        .raw()
        .db()
        .faces()
        .flat_map(|face| face.families.iter().map(|(family, _)| family.clone()))
        .collect()
}
