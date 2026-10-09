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

#[test]
fn where_a_scene_draws_something_is_known_without_drawing_it() {
    let mut scene = Scene::new(Color::TRANSPARENT);
    assert!(!scene.covers(Point::new(5.0, 5.0)), "an empty scene covers nothing");
    scene.shadow(Rect::new(10.0, 10.0, 30.0, 30.0), 0.0, &Shadow { color: Color::BLACK.with_alpha(0.5), blur: 20.0, offset: (0.0, 20.0), spread: 0.0, inset: false });
    scene.fill(Rect::new(10.0, 10.0, 30.0, 30.0), 4.0, red(), None);
    scene.fill(Rect::new(60.0, 10.0, 30.0, 30.0), 0.0, Color::TRANSPARENT, None);
    scene.fill(Rect::new(60.0, 60.0, 30.0, 30.0), 0.0, Color::TRANSPARENT, Some((2.0, blue())));
    scene.push_clip(Rect::new(0.0, 60.0, 20.0, 40.0));
    scene.push_offset(Point::new(0.0, 60.0));
    scene.fill(Rect::new(0.0, 0.0, 50.0, 30.0), 0.0, blue(), None);
    scene.pop_offset();
    scene.pop_clip();
    let cover = scene.cover();
    for (p, covered, what) in [
        (Point::new(20.0, 20.0), true, "a fill"),
        (Point::new(20.0, 50.0), false, "a shadow is not something to click"),
        (Point::new(70.0, 20.0), false, "a fill that cannot be seen"),
        (Point::new(70.0, 70.0), true, "an outline round nothing"),
        (Point::new(10.0, 70.0), true, "inside its clip, where it was moved to"),
        (Point::new(30.0, 70.0), false, "cut off by its clip"),
        (Point::new(95.0, 95.0), false, "nothing at all"),
    ] {
        assert_eq!((cover.covers(p), scene.covers(p)), (covered, covered), "{what}");
    }
}

/// What a 100 by 100 target of `format` holds after it is filled with blue
/// and `scene` is drawn over it: its bytes as they are stored.
fn over_blue(format: wgpu::TextureFormat, scene: &Scene) -> Vec<u8> {
    let mut renderer = Renderer::headless(Fonts::system()).expect("a GPU adapter is required for these tests");
    let side = W as u32;
    let texture = renderer.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("a host's frame"),
        size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    // The host's own frame: blue all over.
    let mut encoder = renderer.device().create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("the host draws"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLUE), store: wgpu::StoreOp::Store } })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    }));
    renderer.queue().submit(Some(encoder.finish()));
    let surface = armature_render::SurfaceTarget { format, unpremultiply: true };
    renderer.render_over(scene, &view, surface, side, side, 1.0);
    // Twice, as a host does frame after frame: the second is laid over the first.
    renderer.render_over(&Scene::new(Color::TRANSPARENT), &view, surface, side, side, 1.0);

    let row = (side * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = renderer.device().create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * side) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut encoder = renderer.device().create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(side) } },
        wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
    );
    renderer.queue().submit(Some(encoder.finish()));
    buffer.map_async(wgpu::MapMode::Read, .., |r| r.expect("map readback buffer"));
    renderer.device().poll(wgpu::PollType::wait_indefinitely()).expect("poll device");
    let data = buffer.get_mapped_range(..).expect("mapped range");
    (0..side).flat_map(|y| data[(y * row) as usize..(y * row + side * 4) as usize].to_vec()).collect()
}

#[test]
fn a_scene_is_drawn_over_what_the_target_already_holds() {
    let mut scene = Scene::new(Color::TRANSPARENT);
    scene.fill(Rect::new(10.0, 10.0, 30.0, 30.0), 0.0, Color::hex(0x00ff00), None);
    scene.fill(Rect::new(60.0, 60.0, 30.0, 30.0), 0.0, red().with_alpha(0.5), None);
    // Stored plainly, the colours are mixed as they stand; on an sRGB target
    // the system mixes light, and an even mix of red and blue is brighter.
    for (format, mixed, swap) in [(wgpu::TextureFormat::Rgba8Unorm, 128, false), (wgpu::TextureFormat::Rgba8UnormSrgb, 188, false), (wgpu::TextureFormat::Bgra8UnormSrgb, 188, true)] {
        let px = over_blue(format, &scene);
        let at = |x: usize, y: usize| {
            let [a, b, c] = at(&px, x, y);
            if swap { [c, b, a] } else { [a, b, c] }
        };
        assert_eq!(at(50, 50), BLUE, "{format:?}: where nothing is drawn, what was there shows");
        assert_eq!(at(95, 5), BLUE, "{format:?}");
        assert_eq!(at(25, 25), [0, 255, 0], "{format:?}: something solid covers it");
        assert!(near(at(75, 75), [mixed, 0, mixed], 3), "{format:?}: something see-through is mixed with it, got {:?}", at(75, 75));
        assert_eq!(px[(50 * W + 50) * 4 + 3], 255, "{format:?}: and the target stays solid");
    }
}
