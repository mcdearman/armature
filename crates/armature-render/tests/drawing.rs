//! What the renderer puts on screen for each drawing call, read back as
//! pixels. Needs a GPU adapter (any Metal, Vulkan, DX12 or GL device,
//! including software rasterisers).

use armature_render::{Color, Fonts, Paint, Point, Rect, Renderer, Scene, Shadow};

const W: usize = 100;
const WHITE: [u8; 3] = [255, 255, 255];
const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

fn red() -> Color {
    Color::hex(0xff0000)
}

fn blue() -> Color {
    Color::hex(0x0000ff)
}

/// A 100 by 100 white scene, drawn by `f` and rendered at `scale`.
fn draw_at(scale: f32, f: impl FnOnce(&mut Scene)) -> Vec<u8> {
    let mut renderer = Renderer::headless(Fonts::system()).expect("a GPU adapter is required for these tests");
    let mut scene = Scene::new(Color::WHITE);
    f(&mut scene);
    let side = (W as f32 * scale) as u32;
    renderer.render_to_rgba(&scene, side, side, scale)
}

fn draw(f: impl FnOnce(&mut Scene)) -> Vec<u8> {
    draw_at(1.0, f)
}

fn at(px: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [px[i], px[i + 1], px[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3], slack: i32) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| (*x as i32 - *y as i32).abs() <= slack)
}

#[test]
fn a_fill_covers_exactly_its_rectangle() {
    let px = draw(|s| s.fill(Rect::new(20.0, 30.0, 40.0, 20.0), 0.0, red(), None));
    assert_eq!((at(&px, 20, 30), at(&px, 59, 49)), (RED, RED), "first and last pixel inside");
    for (x, y) in [(19, 40), (60, 40), (40, 29), (40, 50)] {
        assert_eq!(at(&px, x, y), WHITE, "just outside at {x},{y}");
    }
}

#[test]
fn an_empty_scene_is_its_clear_colour() {
    let px = draw(|_| {});
    assert_eq!(px.len(), W * W * 4);
    assert!(px.chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
}

#[test]
fn later_shapes_cover_earlier_ones() {
    let px = draw(|s| {
        s.fill(Rect::new(10.0, 10.0, 60.0, 60.0), 0.0, red(), None);
        s.fill(Rect::new(40.0, 40.0, 50.0, 50.0), 0.0, blue(), None);
    });
    assert_eq!(at(&px, 20, 20), RED);
    assert_eq!(at(&px, 50, 50), BLUE, "the overlap shows the later shape");
}

#[test]
fn rounded_corners_leave_the_corner_clear() {
    let px = draw(|s| s.fill(Rect::new(10.0, 10.0, 80.0, 80.0), 20.0, red(), None));
    assert_eq!(at(&px, 50, 50), RED);
    assert_eq!(at(&px, 50, 11), RED, "straight edges are full");
    for (x, y) in [(11, 11), (88, 11), (11, 88), (88, 88)] {
        assert_eq!(at(&px, x, y), WHITE, "corner at {x},{y}");
    }
}

#[test]
fn a_border_is_drawn_inside_the_edge() {
    let px = draw(|s| s.fill(Rect::new(10.0, 10.0, 80.0, 80.0), 0.0, red(), Some((4.0, blue()))));
    assert_eq!(at(&px, 11, 50), BLUE);
    assert_eq!(at(&px, 12, 50), BLUE);
    assert_eq!(at(&px, 15, 50), RED, "past the 4px border");
    assert_eq!(at(&px, 9, 50), WHITE, "the border adds no size");
}

#[test]
fn a_clip_limits_drawing_until_it_is_popped() {
    let px = draw(|s| {
        s.push_clip(Rect::new(0.0, 0.0, 50.0, 100.0));
        s.fill(Rect::new(0.0, 0.0, 100.0, 40.0), 0.0, red(), None);
        s.pop_clip();
        s.fill(Rect::new(0.0, 60.0, 100.0, 40.0), 0.0, blue(), None);
    });
    assert_eq!((at(&px, 49, 20), at(&px, 50, 20)), (RED, WHITE));
    assert_eq!(at(&px, 90, 80), BLUE, "drawn after the clip ended");
}

#[test]
fn an_offset_moves_what_is_drawn_inside_it() {
    let px = draw(|s| {
        s.push_offset(Point::new(30.0, 40.0));
        s.fill(Rect::new(0.0, 0.0, 20.0, 20.0), 0.0, red(), None);
        s.pop_offset();
        s.fill(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0, blue(), None);
    });
    assert_eq!((at(&px, 30, 40), at(&px, 49, 59)), (RED, RED));
    assert_eq!(at(&px, 29, 40), WHITE);
    assert_eq!(at(&px, 5, 5), BLUE, "back at the origin after the pop");
}

#[test]
fn a_new_layer_draws_over_everything_before_it() {
    let px = draw(|s| {
        s.push_layer();
        s.fill(Rect::new(0.0, 0.0, 100.0, 100.0), 0.0, blue(), None);
    });
    assert_eq!(at(&px, 50, 50), BLUE);
}

#[test]
fn see_through_colours_blend_in_srgb() {
    // Half black over white is mid grey, as in a browser.
    let px = draw(|s| s.fill(Rect::new(0.0, 0.0, 100.0, 100.0), 0.0, Color::BLACK.with_alpha(0.5), None));
    assert!(near(at(&px, 50, 50), [128, 128, 128], 2), "got {:?}", at(&px, 50, 50));
}

#[test]
fn a_shadow_darkens_around_its_shape_and_fades_out() {
    let shadow = Shadow { offset: (0.0, 0.0), blur: 16.0, spread: 0.0, color: Color::BLACK.with_alpha(0.5), inset: false };
    let px = draw(|s| s.shadow(Rect::new(30.0, 30.0, 40.0, 40.0), 0.0, &shadow));
    let level = |x: usize| at(&px, x, 50)[0];
    assert_eq!(level(50), 255, "like CSS, an outer shadow never paints under its own shape");
    assert!(level(28) < 235, "darkest just outside the edge, got {}", level(28));
    assert!(level(22) > level(28), "and lighter further out");
    assert_eq!(level(2), 255, "gone well away from the shape");
}

#[test]
fn a_shadow_follows_its_offset() {
    let shadow = Shadow { offset: (0.0, 10.0), blur: 8.0, spread: 0.0, color: Color::BLACK.with_alpha(0.6), inset: false };
    let px = draw(|s| s.shadow(Rect::new(30.0, 30.0, 40.0, 30.0), 0.0, &shadow));
    assert!(at(&px, 50, 68)[0] < at(&px, 50, 32)[0], "darker below than above");
}

#[test]
fn a_paint_draws_shadow_fill_and_border_together() {
    let paint = Paint {
        fill: red(),
        border: Some((2.0, blue())),
        shadows: vec![Shadow { offset: (0.0, 0.0), blur: 12.0, spread: 0.0, color: Color::BLACK.with_alpha(0.5), inset: false }],
        content: Color::WHITE,
    };
    let px = draw(|s| s.paint(Rect::new(30.0, 30.0, 40.0, 40.0), 0.0, &paint));
    assert_eq!(at(&px, 50, 50), RED);
    assert_eq!(at(&px, 30, 50), BLUE);
    let outside = at(&px, 27, 50);
    assert!(outside[0] < 250 && outside[0] == outside[2], "a grey shadow outside the edge, got {outside:?}");
}

#[test]
fn a_gradient_runs_between_its_two_points() {
    let px = draw(|s| s.gradient(Rect::new(0.0, 0.0, 100.0, 100.0), 0.0, Color::BLACK, Point::new(0.0, 0.0), Color::WHITE, Point::new(100.0, 0.0)));
    let level = |x: usize| at(&px, x, 50)[0];
    assert!(level(2) < 12 && level(97) > 243, "ends: {} and {}", level(2), level(97));
    assert!(near([level(50); 3], [128; 3], 4), "the middle is half way, got {}", level(50));
    assert_eq!(level(50), at(&px, 50, 5)[0], "constant across the other axis");
}

#[test]
fn a_line_is_as_thick_as_asked() {
    let px = draw(|s| s.line(Point::new(10.0, 50.0), Point::new(90.0, 50.0), 6.0, red()));
    assert_eq!((at(&px, 50, 48), at(&px, 50, 51)), (RED, RED));
    assert_eq!((at(&px, 50, 45), at(&px, 50, 54)), (WHITE, WHITE));
}

#[test]
fn the_scale_factor_multiplies_pixels() {
    let px = draw_at(2.0, |s| s.fill(Rect::new(10.0, 10.0, 20.0, 20.0), 0.0, red(), None));
    assert_eq!(px.len(), 200 * 200 * 4);
    let at2 = |x: usize, y: usize| {
        let i = (y * 200 + x) * 4;
        [px[i], px[i + 1], px[i + 2]]
    };
    assert_eq!((at2(20, 20), at2(59, 59)), (RED, RED));
    assert_eq!((at2(19, 30), at2(60, 30)), (WHITE, WHITE));
}
