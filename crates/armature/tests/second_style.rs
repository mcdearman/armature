//! The framework with a look that is not Neo's. The `retro` example defines
//! its own theme and controls using nothing but this crate; these tests
//! drive it, which is the evidence that the framework imposes no style.
//!
//! Needs a GPU adapter.

#[path = "../examples/retro.rs"]
mod retro;

use armature::testing::Harness;
use armature::{Color, CursorIcon, Key, Modifiers, Point, Scheme, Size};
use retro::{Counter, Retro, DEPTH};

const W: usize = 320;

fn harness() -> Harness<Counter> {
    Harness::new(Counter::default(), Size::new(W as f32, 180.0)).expect("a GPU adapter is required for these tests")
}

fn at(px: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [px[i], px[i + 1], px[i + 2]]
}

fn is(got: [u8; 3], c: Color) -> bool {
    let want = c.to_rgba8();
    got.iter().zip(want.iter()).all(|(a, b)| (*a as i32 - *b as i32).abs() <= 3)
}

/// The top-left corner of the first button's face: scanning along the
/// diagonal from the window corner, past the text, to the first ink pixel.
fn first_button(px: &[u8], theme: Retro) -> (usize, usize) {
    let y = (60..170).find(|y| is(at(px, 30, *y), theme.ink)).expect("a button outline below the text");
    (24, y)
}

#[test]
fn the_apps_own_theme_paints_the_window_and_its_controls() {
    let mut h = harness();
    let theme = *h.style().get::<Retro>().expect("the style carries the app's theme");
    assert_eq!(theme, Retro::for_scheme(Scheme::Light));
    let px = h.render(1.0);
    assert!(is(at(&px, 310, 170), theme.paper), "the window is paper");
    let (x, y) = first_button(&px, theme);
    assert!(is(at(&px, x + 1, y + 1), theme.ink), "a hard outline");
    assert!(is(at(&px, x + 6, y + 6), theme.pop), "filled with the pop colour, got {:?}", at(&px, x + 6, y + 6));
    assert!(is(at(&px, x, y), theme.ink) && !is(at(&px, x - 2, y), theme.ink), "square corners: ink right into the corner");
}

#[test]
fn the_look_follows_the_system_scheme() {
    let mut h = harness();
    let light = h.render(1.0);
    h.set_system_scheme(Scheme::Dark);
    let theme = *h.style().get::<Retro>().unwrap();
    assert_eq!(theme, Retro::for_scheme(Scheme::Dark));
    let dark = h.render(1.0);
    assert!(is(at(&dark, 310, 170), theme.paper));
    assert_ne!(at(&light, 310, 170), at(&dark, 310, 170));
}

#[test]
fn its_controls_work_with_pointer_and_keyboard() {
    let mut h = harness();
    let theme = Retro::for_scheme(Scheme::Light);
    let (x, y) = first_button(&h.render(1.0), theme);
    let less = Point::new(x as f32 + 20.0, y as f32 + 15.0);
    h.click(less);
    h.click(less);
    assert_eq!(h.app().count, -2);
    assert_eq!(h.cursor(), CursorIcon::Pointer);

    // Tab reaches the first button, then the second.
    h.key(Key::Tab, Modifiers::default());
    h.key(Key::Tab, Modifiers::default());
    h.key(Key::Enter, Modifiers::default());
    assert_eq!(h.app().count, -1);
}

#[test]
fn a_held_button_sinks_onto_its_shadow() {
    let mut h = harness();
    let theme = Retro::for_scheme(Scheme::Light);
    let rest = h.render(1.0);
    let (x, y) = first_button(&rest, theme);
    let inside = Point::new(x as f32 + 20.0, y as f32 + 15.0);
    h.move_to(inside);
    h.event(armature::Event::PointerPressed { pos: inside, button: armature::PointerButton::Primary });
    let held = h.render(1.0);
    // At rest the face starts at the corner; held, it has moved by DEPTH
    // and the corner shows paper.
    assert!(is(at(&rest, x, y), theme.ink));
    assert!(is(at(&held, x, y), theme.paper));
    let d = DEPTH as usize;
    assert!(is(at(&held, x + d, y + d), theme.ink));
}

#[test]
fn strings_become_labels_in_the_styles_text_colour() {
    let mut h = harness();
    let theme = Retro::for_scheme(Scheme::Light);
    let px = h.render(1.0);
    // Somewhere in the first line of text there is ink.
    let inked = (24..200).any(|x| (24..50).any(|y| is(at(&px, x, y), theme.ink)));
    assert!(inked, "the count is drawn in ink");
}
