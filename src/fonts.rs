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

//! # The default font
//!
//! An app written for iced knows nothing about this board's fonts, and would draw no text at all
//! if it had to install one. So this crate carries its own: [`install_default`] puts a 16 KiB
//! Latin subset of Roboto (see `fonts/README.md`) in the database unless something else already
//! did, and the host calls it on the way in — [`Application::new`](crate::Application::new) and
//! [`Host::new`](crate::Host::new).
//!
//! Two rules, and the order between them is the whole design:
//!
//! * [`install`] always wins. It is how an app or a firmware says "this is the typeface", and the
//!   face it installs becomes the *sans-serif* family — which is what `Font::default()` resolves
//!   to — so the last explicit install is what every unqualified label gets.
//! * [`install_default`] only fills a gap. It is a no-op the moment anything has been installed,
//!   so an app that installs its own font in `boot()` keeps it, whichever order the two happen to
//!   run in.
//!
//! A program can also bring fonts through iced's own channel — `Program::settings().fonts` — and
//! [`Host::new`](crate::Host::new) loads those. They are extra faces to name explicitly
//! (`Font::with_name`), which is iced's meaning for them, and they do not touch the default family.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

use iced_graphics::text::cosmic_text::fontdb::Family;

/// What this crate embeds, so that a program which installs nothing still draws text.
const DEFAULT: &[u8] = include_bytes!("../fonts/Roboto-Subset.ttf");

/// Whether anyone has installed a font yet — an app, a firmware, or [`install_default`] itself.
static INSTALLED: AtomicBool = AtomicBool::new(false);

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

    INSTALLED.store(true, Ordering::Relaxed);
}

/// Installs this crate's own subset, unless an app or a firmware installed a font first.
///
/// The host calls this at both entry points, which is what lets a program written for iced show
/// text without knowing anything about fonts. See the module docs for the rule.
pub fn install_default() {
    if installed() {
        return;
    }

    install(DEFAULT);
}

/// Whether a font has been installed by anyone — an app, a firmware, or this crate's default.
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// The family `Font::default()` resolves to, which is the sans-serif one.
///
/// For diagnostics, and the reason [`install`] sets it: a device that renders the wrong glyphs
/// should be asked what it actually has.
pub fn sans_serif() -> String {
    // A write guard, not a read one: iced only exposes `raw()` mutably.
    let mut system = iced_graphics::text::font_system()
        .write()
        .expect("write font system");

    system.raw().db().family_name(&Family::SansSerif).to_owned()
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
