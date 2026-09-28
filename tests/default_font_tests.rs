//! What a program gets when it installs no font.
//!
//! The point of the default is that a program written for iced — which knows nothing about this
//! board, and whose author has never heard of `fonts::install` — still draws text. The other half
//! of it is that the default must not *take over*: an app or a firmware that installs its own font
//! has to keep it, whichever order the two calls happen in.
//!
//! Both halves are asserted here, in **one** test on purpose: the font system is a process-wide
//! global, so a second test running beside this one would share it, and "nothing was installed
//! yet" would stop being true.
//!
//! `tests/fonts/Another-Font.ttf` stands in for an app's own font: four glyphs of DejaVu Sans
//! ("Text", the word this test draws), 14 KiB. It is a *cut* rather than the whole 756 KiB face
//! precisely because the last section of this test asserts that a different font draws different
//! pixels — and shipping a whole font to prove that would be the opposite of what this crate
//! does about fonts.

use iced_core::theme::{Base, Mode};
use iced_core::{Element, Theme};
use iced_widget::{container, text};
use iced_winit::{fonts, App, Application, Renderer};
use pomelo_gfx::rgb888_to_rgb565;

/// An app that draws a word and nothing else. Its renderer is the recorded one when the `renderer`
/// feature is on, and `tiny-skia` otherwise — the font system is the same either way.
struct Label;

impl App for Label {
    type Message = ();
    type Theme = Theme;

    fn theme(&self) -> Theme {
        <Theme as Base>::default(Mode::Dark)
    }

    fn update(&mut self, _: ()) {}

    fn view(&self) -> Element<'_, (), Theme, Renderer> {
        container(text("Text").size(22)).padding(8).into()
    }
}

/// Someone else's font, for the "an explicit install wins" half.
const ANOTHER_FONT: &[u8] = include_bytes!("fonts/Another-Font.ttf");

const SIZE: u32 = 128;

/// Draws one frame and returns the panel, plus the background colour, so that "did anything get
/// painted" is not a guess about a particular pixel value.
fn render() -> (Vec<u16>, u16) {
    let mut app = Application::new(Label, SIZE, SIZE).expect("the panel's buffers");

    assert_eq!(app.frame().len(), 1, "the first frame draws everything");

    let background = <Theme as Base>::default(Mode::Dark).base().background_color;
    let [r, g, b, _] = background.into_rgba8();

    (app.panel().data().to_vec(), rgb888_to_rgb565(r, g, b))
}

#[test]
fn the_default_fills_the_gap_and_an_explicit_font_keeps_it() {
    // Nothing has been installed: no app has run, and the host has not started.
    assert!(
        !fonts::installed(),
        "a fresh process starts with no font installed"
    );

    // Entering the host is enough to get one, and `Font::default()` — what every unqualified label
    // asks for — resolves to it.
    let (default, background) = render();

    assert!(fonts::installed(), "Application::new installs a default");
    assert_eq!(
        fonts::sans_serif(),
        "Roboto",
        "the default has to become the sans-serif family, or labels draw as glyphs of nothing"
    );
    assert!(
        default.iter().filter(|pixel| **pixel != background).count() > 0,
        "and the label has to draw something"
    );

    // Now an app installs its own font, the way an app with a house typeface would.
    fonts::install(ANOTHER_FONT);

    assert_eq!(
        fonts::sans_serif(),
        "DejaVu Sans",
        "an explicit install is what `Font::default()` resolves to from here on"
    );

    let (installed, _) = render();

    assert_ne!(
        default, installed,
        "a different typeface has to draw different pixels, or none of this means anything"
    );

    // And the default stays out of the way afterwards, however late the host gets there.
    let faces = fonts::families().len();
    fonts::install_default();

    assert_eq!(
        fonts::sans_serif(),
        "DejaVu Sans",
        "the default must not take back the family"
    );
    assert_eq!(
        fonts::families().len(),
        faces,
        "nor put a second face in the database"
    );

    // From here on `installed()` is true, which is what makes the assertions above repeatable:
    // another `install_default` cannot change anything.
    let (again, _) = render();

    assert_eq!(
        installed, again,
        "nothing changed, so nothing should redraw"
    );
}
