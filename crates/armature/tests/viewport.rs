//! A game shown in a window: an app that draws its own frame into a
//! texture, on the device the window is drawn with, and shows it in a
//! `viewport`, which tells it how large to draw and passes on its input.
//!
//! Needs a GPU adapter.

use std::time::{Duration, Instant};

use armature::testing::Harness;
use armature::widgets::{row, viewport, Space, ViewportEvent};
use armature::{wgpu, App, Element, Event, Graphics, Image, Key, KeyEvent, Length, Modifiers, Point, PointerButton, Rect, Size, Status, WindowRequest};

const W: usize = 300;

#[derive(Clone, Debug)]
enum Msg {
    Resized(Rect, f32),
    Input(ViewportEvent),
}

/// A game of the simplest kind: each frame it fills its picture with one
/// colour, the next from a list.
#[derive(Default)]
struct Game {
    graphics: Option<Graphics>,
    /// What it draws into, and that as a picture to show.
    target: Option<(wgpu::Texture, Image)>,
    size: (u32, u32),
    frames: u32,
    steps: Vec<Duration>,
    playing: bool,
    placed: Vec<(Rect, f32)>,
    heard: Vec<ViewportEvent>,
}

/// The colours of its frames, as light: red, a mid grey, green.
const FRAMES: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.2158605, 0.2158605, 0.2158605], [0.0, 1.0, 0.0]];

impl App for Game {
    type Message = Msg;

    fn wanted_features(&self, available: wgpu::Features) -> wgpu::Features {
        // Something every adapter may not have, and something none has.
        available & wgpu::Features::TIMESTAMP_QUERY
    }

    fn graphics(&mut self, graphics: &Graphics) {
        self.graphics = Some(graphics.clone());
    }

    fn step(&mut self, _now: Instant, dt: Duration) -> bool {
        self.steps.push(dt);
        let (Some(g), (w, h)) = (&self.graphics, self.size) else { return false };
        if w == 0 || h == 0 {
            return false;
        }
        // A new texture when the size has changed, and a new picture of it.
        if self.target.as_ref().is_none_or(|(t, _)| (t.width(), t.height()) != (w, h)) {
            let texture = g.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("the game's frame"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
            });
            // Read as the bytes that are stored, which are sRGB-encoded.
            let shown = texture.create_view(&wgpu::TextureViewDescriptor { format: Some(wgpu::TextureFormat::Rgba8Unorm), ..Default::default() });
            self.target = Some((texture, Image::from_texture(shown, w, h)));
        }
        let (texture, _) = self.target.as_ref().expect("made above");
        let [r, gr, b] = FRAMES[self.frames as usize % FRAMES.len()];
        self.frames += 1;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("the game draws"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r, g: gr, b, a: 1.0 }), store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        g.queue.submit(Some(encoder.finish()));
        true
    }

    fn update(&mut self, m: Msg) {
        match m {
            Msg::Resized(bounds, scale) => {
                self.size = ((bounds.w * scale).round() as u32, (bounds.h * scale).round() as u32);
                self.placed.push((bounds, scale));
            }
            Msg::Input(e) => self.heard.push(e),
        }
    }

    fn view(&self) -> Element<Msg> {
        // The game on the left, and room beside it that is not the game's.
        row().push(Element::new(viewport(self.target.as_ref().map(|(_, image)| image)).width(200.0).height(Length::Fill).on_resize(Msg::Resized).on_input(Msg::Input).playing(self.playing).capture(true))).push(Element::new(Space::new(100.0, 10.0))).into()
    }
}

fn at(px: &[u8], scale: usize, x: usize, y: usize) -> [u8; 3] {
    let i = (y * scale * W * scale + x * scale) * 4;
    [px[i], px[i + 1], px[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| (*x as i32 - *y as i32).abs() <= 2)
}

fn key(key: Key, pressed: bool) -> Event {
    Event::Key(KeyEvent { key, pressed, repeat: false, modifiers: Modifiers::default(), text: None })
}

const TICK: Duration = Duration::from_millis(16);

#[test]
fn a_game_draws_its_own_frame_on_the_windows_device_and_it_is_shown() {
    let mut h = Harness::new(Game::default(), Size::new(W as f32, 120.0)).expect("a GPU adapter is required for these tests");
    let g = h.app().graphics.clone().expect("handed the device before the first view");
    assert!(!g.features.contains(wgpu::Features::EXPERIMENTAL_RAY_QUERY) && g.limits.max_texture_dimension_2d >= 2048, "what it was made with is said");

    // The first frame finds out how large the viewport is: there is nothing to show yet.
    let px = h.frame(Duration::ZERO, 2.0);
    assert_eq!(h.app().placed, [(Rect::new(0.0, 0.0, 200.0, 120.0), 2.0)], "where it is, and the screen's scale");
    assert_eq!((h.app().size, at(&px, 2, 100, 60)), ((400, 240), [0, 0, 0]));
    // From then on each frame shows what the game drew for that very frame, not the one before.
    let px = h.frame(TICK, 2.0);
    assert!(near(at(&px, 2, 100, 60), [255, 0, 0]) && near(at(&px, 2, 199, 119), [255, 0, 0]), "the game's first frame fills the viewport: {:?}", at(&px, 2, 100, 60));
    assert_eq!(at(&px, 2, 250, 60)[..], h.frame(Duration::ZERO, 2.0)[(60 * 2 * W * 2 + 250 * 2) * 4..][..3], "and nothing beside it");
    let px = h.frame(TICK, 2.0);
    assert!(near(at(&px, 2, 100, 60), [0, 255, 0]), "{:?}", at(&px, 2, 100, 60));
    // A colour between: what the game drew as light is shown as it would be on a screen.
    let mut h = Harness::new(Game::default(), Size::new(W as f32, 120.0)).unwrap();
    h.frame(Duration::ZERO, 1.0);
    h.frame(TICK, 1.0);
    let px = h.frame(TICK, 1.0);
    assert!(near(at(&px, 1, 100, 60), [128, 128, 128]), "{:?}", at(&px, 1, 100, 60));
    assert_eq!(h.app().steps, [Duration::ZERO, TICK, TICK], "stepped before each frame, with the time since the last");

    // A new size, or a screen of another scale: told, and the picture made anew to fit.
    h.resize(Size::new(W as f32, 60.0));
    h.frame(TICK, 1.0);
    let px = h.frame(TICK, 1.0);
    assert_eq!((h.app().placed.last().copied(), h.app().size), (Some((Rect::new(0.0, 0.0, 200.0, 60.0), 1.0)), (200, 60)));
    assert!(!near(at(&px, 1, 100, 30), [0, 0, 0]));

    // Still, it is drawn when something changes; playing, every frame.
    assert!(!h.wants_frame());
    h.app_mut().playing = true;
    h.frame(TICK, 1.0);
    h.frame(TICK, 1.0);
    assert!(h.wants_frame());
}

#[test]
fn a_viewport_takes_the_keyboard_when_clicked_and_passes_on_what_happens_in_it() {
    let mut h = Harness::new(Game::default(), Size::new(W as f32, 120.0)).expect("a GPU adapter is required for these tests");
    h.frame(Duration::ZERO, 1.0);
    // Before it is clicked, keys are not the game's.
    assert_eq!((h.event(key(Key::Character("w".into()), true)), h.app().heard.len()), (Status::Ignored, 0));

    // A click in it: it has the keyboard, holds the pointer, and hears the click where it fell.
    let inside = Point::new(50.0, 40.0);
    assert_eq!(h.event(Event::PointerPressed { pos: inside, button: PointerButton::Primary }), Status::Captured);
    assert_eq!(h.app().heard, [ViewportEvent::Focused(true), ViewportEvent::Captured(true), ViewportEvent::Pressed(inside, PointerButton::Primary)]);
    assert_eq!(h.take_window_requests(), [WindowRequest::CapturePointer(true)]);
    h.app_mut().heard.clear();
    h.event(Event::PointerReleased { pos: inside, button: PointerButton::Primary });
    assert_eq!(h.event(key(Key::Character("w".into()), true)), Status::Captured);
    h.event(Event::PointerMotion { delta: Point::new(4.0, -2.0) });
    h.event(Event::Wheel { pos: inside, delta: Point::new(0.0, 3.0) });
    assert_eq!(h.app().heard.iter().map(|e| format!("{e:?}").split(['(', ' ']).next().unwrap().to_owned()).collect::<Vec<_>>(), ["Released", "Key", "Motion", "Wheel"]);
    assert_eq!(h.app().heard[2], ViewportEvent::Motion(Point::new(4.0, -2.0)));

    // Escape lets the pointer go and is not the game's; everything held is taken as let go.
    h.app_mut().heard.clear();
    assert_eq!(h.event(key(Key::Escape, true)), Status::Captured);
    assert_eq!((h.app().heard.clone(), h.take_window_requests()), (vec![ViewportEvent::Captured(false), ViewportEvent::AllReleased], vec![WindowRequest::CapturePointer(false)]));
    // It still has the keyboard: Escape now is a key like any other, and the pointer is where it is.
    h.app_mut().heard.clear();
    h.event(key(Key::Escape, true));
    h.event(Event::PointerMoved { pos: Point::new(60.0, 50.0) });
    h.event(Event::PointerMotion { delta: Point::new(9.0, 9.0) });
    assert_eq!(h.app().heard.len(), 2, "{:?}", h.app().heard);
    assert_eq!(h.app().heard[1], ViewportEvent::Moved(Point::new(60.0, 50.0)));

    // A click beside it: the keyboard goes, and the game is told all is let go.
    h.app_mut().heard.clear();
    h.event(Event::PointerPressed { pos: Point::new(250.0, 40.0), button: PointerButton::Primary });
    assert_eq!(h.app().heard, [ViewportEvent::AllReleased, ViewportEvent::Focused(false)]);
    assert_eq!(h.event(key(Key::Character("w".into()), true)), Status::Ignored);

    // Held again, and the window itself loses the keyboard: let go, and told so.
    h.event(Event::PointerPressed { pos: inside, button: PointerButton::Primary });
    h.take_window_requests();
    h.app_mut().heard.clear();
    h.event(Event::WindowFocus(false));
    assert_eq!((h.app().heard.clone(), h.take_window_requests()), (vec![ViewportEvent::Captured(false), ViewportEvent::AllReleased], vec![WindowRequest::CapturePointer(false)]));
}
