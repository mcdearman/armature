//! Connects a [`Ui`] to a real window with winit and wgpu.

use std::sync::Arc;
use std::time::{Duration, Instant};

use armature_render::{wgpu, Point, Renderer, Size, SurfaceTarget};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{ResizeDirection, Window, WindowId, WindowLevel};

use crate::app::{App, Decorations, Scheme};
use crate::core::{CursorIcon, ResizeEdge, WindowRequest};
use crate::event::{Event, Key, KeyEvent, Modifiers, PointerButton};
use crate::runtime::Ui;

/// Errors that stop an application from starting.
#[derive(Debug)]
pub enum Error {
    EventLoop(winit::error::EventLoopError),
    Graphics(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::EventLoop(e) => write!(f, "event loop: {e}"),
            Error::Graphics(e) => write!(f, "graphics: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Opens a window and runs `app` until it closes.
pub fn run<A: App>(app: A) -> Result<(), Error> {
    let event_loop = EventLoop::new().map_err(Error::EventLoop)?;
    let settings = app.window();
    let mut ui = Ui::new(app, settings.size, Scheme::Light);
    let waker = event_loop.create_proxy();
    ui.start(Arc::new(move || {
        let _ = waker.send_event(());
    }));
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        ui.set_clipboard_reader(Box::new(move || clipboard.get_text().ok()));
    }
    let mut shell = Shell {
        ui,
        settings,
        gpu: None,
        error: None,
        modifiers: ModifiersState::empty(),
        pointer: None,
        clipboard: arboard::Clipboard::new().ok(),
        glass: None,
        close: false,
        blur_strength: None,
        blur_attempts: 0,
        hidden: false,
        retry_at: None,
        state: None,
        resizing: None,
        dropped: vec![],
    };
    event_loop.run_app(&mut shell).map_err(Error::EventLoop)?;
    match shell.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

struct Gpu {
    // Field order matters: the surface must drop before the window.
    renderer: Renderer,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    target: SurfaceTarget,
    instance: wgpu::Instance,
    window: Arc<Window>,
}

struct Shell<A: App> {
    ui: Ui<A>,
    settings: crate::app::WindowSettings,
    gpu: Option<Gpu>,
    error: Option<Error>,
    modifiers: ModifiersState,
    pointer: Option<Point>,
    clipboard: Option<arboard::Clipboard>,
    /// Glass state last applied to the window: enabled and corner radius.
    glass: Option<(bool, f32)>,
    close: bool,
    /// Blur strength last applied, and how many frames we have waited for
    /// the platform's blur layers to appear.
    blur_strength: Option<f32>,
    blur_attempts: u32,
    /// The window is fully covered, minimised or on a sleeping display, so
    /// nothing is drawn until it is visible again.
    hidden: bool,
    /// The surface had no frame to give; try again at this time rather than
    /// straight away, which would spin a processor core.
    retry_at: Option<Instant>,
    /// The app's window state as last applied to the window.
    state: Option<crate::app::WindowState>,
    /// An edge drag the framework is carrying out itself, where the platform cannot.
    resizing: Option<ManualResize>,
    /// Files let go over the window, gathered until the batch is complete.
    dropped: Vec<std::path::PathBuf>,
}

/// A resize in progress: the edge held, where the pointer grabbed it on the
/// screen, and the window's frame at that moment, all in physical pixels.
struct ManualResize {
    edge: ResizeEdge,
    grab: (f64, f64),
    origin: (f64, f64),
    size: (f64, f64),
}

fn scheme_of(t: winit::window::Theme) -> Scheme {
    match t {
        winit::window::Theme::Dark => Scheme::Dark,
        winit::window::Theme::Light => Scheme::Light,
    }
}

impl<A: App> Shell<A> {
    fn create(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Error> {
        let s = &self.settings;
        let mut attrs = Window::default_attributes()
            .with_title(self.ui.title())
            .with_inner_size(LogicalSize::new(s.size.w as f64, s.size.h as f64))
            .with_transparent(true)
            .with_decorations(s.decorations == Decorations::System)
            .with_resizable(s.resizable)
            // An app that starts in the background never flashes a window.
            .with_visible(self.ui.window_state().visible);
        if let Some(m) = s.min_size {
            attrs = attrs.with_min_inner_size(LogicalSize::new(m.w as f64, m.h as f64));
        }
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android"), not(target_os = "ios")))]
        if let Some(id) = &s.app_id {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs = WindowAttributesExtWayland::with_name(attrs, id.clone(), id.clone());
            use winit::platform::x11::WindowAttributesExtX11;
            attrs = WindowAttributesExtX11::with_name(attrs, id.clone(), id.clone());
        }
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            if s.decorations == Decorations::Custom {
                // Keep the native shadow and resize edges while the app draws the chrome.
                attrs = attrs.with_decorations(true).with_titlebar_transparent(true).with_title_hidden(true).with_fullsize_content_view(true).with_titlebar_buttons_hidden(true);
            }
        }
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| Error::Graphics(e.to_string()))?);
        if let Some(t) = window.theme() {
            self.ui.set_system_scheme(scheme_of(t));
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(event_loop.owned_display_handle())));
        let surface = instance.create_surface(window.clone()).map_err(|e| Error::Graphics(e.to_string()))?;
        let (adapter, device, queue) = pollster::block_on(async {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions { compatible_surface: Some(&surface), ..Default::default() })
                .await
                .map_err(|e| Error::Graphics(format!("no GPU adapter: {e}")))?;
            let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor::default()).await.map_err(|e| Error::Graphics(e.to_string()))?;
            Ok::<_, Error>((adapter, device, queue))
        })?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).or_else(|| caps.formats.first().copied()).ok_or_else(|| Error::Graphics("surface has no formats".into()))?;
        let (alpha_mode, unpremultiply) = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
            (wgpu::CompositeAlphaMode::PreMultiplied, false)
        } else if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PostMultiplied) {
            // Metal reports this mode, but Core Animation composites the layer
            // as premultiplied, so on macOS the canvas goes out unchanged.
            (wgpu::CompositeAlphaMode::PostMultiplied, !cfg!(target_os = "macos"))
        } else {
            (caps.alpha_modes[0], false)
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);
        let scale = window.scale_factor() as f32;
        self.ui.resize(Size::new(size.width as f32 / scale, size.height as f32 / scale));
        self.gpu = Some(Gpu {
            renderer: Renderer::new(device, queue, self.ui.app().fonts()),
            surface,
            config,
            target: SurfaceTarget { format, unpremultiply },
            instance,
            window,
        });
        self.report_frame();
        self.sync_window();
        if let Some(gpu) = &self.gpu {
            gpu.window.request_redraw();
        }
        Ok(())
    }

    /// Applies style-driven window state such as backdrop blur.
    fn sync_window(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        let state = self.ui.window_state();
        if self.state != Some(state) {
            let old = self.state.replace(state);
            if old.map(|o| o.always_on_top) != Some(state.always_on_top) {
                gpu.window.set_window_level(if state.always_on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
            }
            if old.map(|o| o.bare) != Some(state.bare) {
                // A see-through window's shadow would outline whatever it draws.
                #[cfg(target_os = "macos")]
                {
                    use winit::platform::macos::WindowExtMacOS;
                    gpu.window.set_has_shadow(!state.bare);
                }
                crate::platform::set_square(&gpu.window, state.bare);
            }
            if old.and_then(|o| o.size) != state.size
                && let Some(s) = state.size
            {
                let _ = gpu.window.request_inner_size(LogicalSize::new(s.w as f64, s.h as f64));
            }
            if old.and_then(|o| o.position) != state.position
                && let Some(p) = state.position
            {
                gpu.window.set_outer_position(winit::dpi::LogicalPosition::new(p.x as f64, p.y as f64));
            }
            if old.map(|o| o.hidden_from_capture) != Some(state.hidden_from_capture) {
                gpu.window.set_content_protected(state.hidden_from_capture);
            }
            if old.map(|o| o.visible) != Some(state.visible) {
                gpu.window.set_visible(state.visible);
                if state.visible {
                    // Windows shown from a global shortcut should come to the front.
                    gpu.window.focus_window();
                    gpu.window.request_redraw();
                }
            }
        }
        // Platform blur would fill a see-through window.
        let glass = (self.ui.backdrop_blur().is_some() && !state.bare, if state.bare { 0.0 } else { self.ui.window_radius() });
        if self.glass != Some(glass) {
            crate::platform::set_blur(&gpu.window, glass.0, glass.1);
            self.glass = Some(glass);
            self.blur_strength = None;
            self.blur_attempts = 0;
        }
        let title = self.ui.title();
        if gpu.window.title() != title {
            gpu.window.set_title(&title);
        }
    }

    /// Applies the style's blur strength once the platform blur exists.
    fn sync_blur_strength(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        let Some(want) = self.ui.backdrop_blur() else {
            return;
        };
        if self.blur_strength == Some(want) {
            return;
        }
        if crate::platform::set_blur_strength(&gpu.window, want) {
            self.blur_strength = Some(want);
        } else if self.blur_attempts < 60 {
            // The system builds its glass layers after a display pass.
            self.blur_attempts += 1;
            gpu.window.request_redraw();
        }
    }

    /// Whether a redraw request would lead to a frame.
    fn can_draw(&self) -> bool {
        // A window the app has hidden gets no frames, so asking for one
        // would never be satisfied and the loop would spin.
        !self.hidden && self.retry_at.is_none() && self.state.is_none_or(|s| s.visible)
    }

    /// Tells the app where its window is on the screen.
    fn report_frame(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        let scale = gpu.window.scale_factor() as f32;
        let size = gpu.window.inner_size();
        // Wayland does not tell clients where their windows are.
        let pos = gpu.window.inner_position().unwrap_or_default();
        let frame = armature_render::Rect::new(pos.x as f32 / scale, pos.y as f32 / scale, size.width as f32 / scale, size.height as f32 / scale);
        let screen = gpu.window.current_monitor().map_or(frame, |m| {
            let (p, s) = (m.position(), m.size());
            armature_render::Rect::new(p.x as f32 / scale, p.y as f32 / scale, s.width as f32 / scale, s.height as f32 / scale)
        });
        self.ui.window_geometry(crate::app::WindowGeometry { frame, screen, scale });
    }

    fn render(&mut self) {
        if self.hidden || self.state.is_some_and(|s| !s.visible) {
            return;
        }
        let Some(gpu) = &mut self.gpu else { return };
        let frame = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            status @ (wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded) => {
                // Asking again at once would loop at full speed while the window
                // is covered. Wait; `Occluded(false)` also restarts drawing.
                let wait = if matches!(status, wgpu::CurrentSurfaceTexture::Occluded) { Duration::from_secs(1) } else { Duration::from_millis(100) };
                self.retry_at = Some(Instant::now() + wait);
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                gpu.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                match gpu.instance.create_surface(gpu.window.clone()) {
                    Ok(s) => gpu.surface = s,
                    Err(e) => eprintln!("armature: could not recreate surface: {e}"),
                }
                gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                gpu.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("armature: surface validation error");
                return;
            }
        };
        self.retry_at = None;
        let scale = gpu.window.scale_factor() as f32;
        let scene = self.ui.draw(gpu.renderer.text(), Instant::now());
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        gpu.window.pre_present_notify();
        gpu.renderer.render(&scene, &view, gpu.target, gpu.config.width, gpu.config.height, scale);
        gpu.renderer.queue().present(frame);
        // Widgets may copy while laying out (for example Vim yanks).
        if let Some(t) = self.ui.take_clipboard()
            && let Some(c) = &mut self.clipboard {
                let _ = c.set_text(t);
            }
        self.sync_blur_strength();
    }

    fn dispatch(&mut self, event: Event) {
        let Some(gpu) = &mut self.gpu else { return };
        self.ui.event(gpu.renderer.text(), event);
        self.after_input();
    }

    fn after_input(&mut self) {
        let Some(gpu) = &self.gpu else { return };
        let mut drag_began = false;
        for r in self.ui.take_window_requests() {
            match r {
                WindowRequest::Drag => {
                    let _ = gpu.window.drag_window();
                }
                WindowRequest::DragFiles(paths) => {
                    // The system runs the drag and swallows the mouse-up, so
                    // tell the widgets the pointer has gone.
                    if crate::platform::drag_files(&gpu.window, &paths) {
                        drag_began = true;
                    }
                }
                WindowRequest::Resize(edge) => {
                    // macOS has no call to start an edge drag, so follow the pointer here.
                    if gpu.window.drag_resize_window(resize_dir(edge)).is_err()
                        && let (Some(p), Ok(origin)) = (self.pointer, gpu.window.outer_position())
                    {
                        let scale = gpu.window.scale_factor();
                        let size = gpu.window.inner_size();
                        let origin = (origin.x as f64, origin.y as f64);
                        self.resizing = Some(ManualResize { edge, grab: (origin.0 + p.x as f64 * scale, origin.1 + p.y as f64 * scale), origin, size: (size.width as f64, size.height as f64) });
                    }
                }
                WindowRequest::Minimize => gpu.window.set_minimized(true),
                WindowRequest::ToggleMaximize => gpu.window.set_maximized(!gpu.window.is_maximized()),
                WindowRequest::Close => {
                    if self.ui.close_requested() {
                        self.close = true;
                    }
                }
            }
        }
        if let Some(t) = self.ui.take_clipboard()
            && let Some(c) = &mut self.clipboard {
                let _ = c.set_text(t);
            }
        gpu.window.set_cursor(cursor_icon(self.ui.cursor()));
        self.sync_window();
        if let Some(gpu) = &self.gpu
            && self.ui.needs_redraw()
            && self.can_draw() {
                gpu.window.request_redraw();
            }
        if drag_began {
            self.pointer = None;
            self.dispatch(Event::PointerLeft);
        }
    }

    fn modifiers(&self) -> Modifiers {
        let m = self.modifiers;
        Modifiers { shift: m.shift_key(), ctrl: m.control_key(), alt: m.alt_key(), logo: m.super_key() }
    }

    /// With custom decorations on Linux and Windows, the window edges resize.
    fn resize_edge(&self, p: Point) -> Option<ResizeEdge> {
        if cfg!(target_os = "macos") || self.settings.decorations != Decorations::Custom || !self.settings.resizable {
            return None;
        }
        let gpu = self.gpu.as_ref()?;
        if gpu.window.is_maximized() {
            return None;
        }
        let scale = gpu.window.scale_factor() as f32;
        let (w, h) = (gpu.config.width as f32 / scale, gpu.config.height as f32 / scale);
        let m = 6.0;
        let (l, r, t, b) = (p.x < m, p.x > w - m, p.y < m, p.y > h - m);
        Some(match (l, r, t, b) {
            (true, _, true, _) => ResizeEdge::NorthWest,
            (_, true, true, _) => ResizeEdge::NorthEast,
            (true, _, _, true) => ResizeEdge::SouthWest,
            (_, true, _, true) => ResizeEdge::SouthEast,
            (true, ..) => ResizeEdge::West,
            (_, true, ..) => ResizeEdge::East,
            (_, _, true, _) => ResizeEdge::North,
            (_, _, _, true) => ResizeEdge::South,
            _ => return None,
        })
    }
}

fn resize_dir(e: ResizeEdge) -> ResizeDirection {
    match e {
        ResizeEdge::North => ResizeDirection::North,
        ResizeEdge::South => ResizeDirection::South,
        ResizeEdge::East => ResizeDirection::East,
        ResizeEdge::West => ResizeDirection::West,
        ResizeEdge::NorthEast => ResizeDirection::NorthEast,
        ResizeEdge::NorthWest => ResizeDirection::NorthWest,
        ResizeEdge::SouthEast => ResizeDirection::SouthEast,
        ResizeEdge::SouthWest => ResizeDirection::SouthWest,
    }
}

fn cursor_icon(c: CursorIcon) -> winit::window::CursorIcon {
    use winit::window::CursorIcon as W;
    match c {
        CursorIcon::Default => W::Default,
        CursorIcon::Pointer => W::Pointer,
        CursorIcon::Text => W::Text,
        CursorIcon::Grab => W::Grab,
        CursorIcon::Grabbing => W::Grabbing,
        CursorIcon::Resize(e) => match e {
            ResizeEdge::North | ResizeEdge::South => W::NsResize,
            ResizeEdge::East | ResizeEdge::West => W::EwResize,
            ResizeEdge::NorthEast | ResizeEdge::SouthWest => W::NeswResize,
            ResizeEdge::NorthWest | ResizeEdge::SouthEast => W::NwseResize,
        },
    }
}

fn map_key(k: &WKey) -> Key {
    match k {
        WKey::Named(n) => match n {
            NamedKey::Enter => Key::Enter,
            NamedKey::Space => Key::Space,
            NamedKey::Tab => Key::Tab,
            NamedKey::Escape => Key::Escape,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Delete => Key::Delete,
            NamedKey::ArrowLeft => Key::Left,
            NamedKey::ArrowRight => Key::Right,
            NamedKey::ArrowUp => Key::Up,
            NamedKey::ArrowDown => Key::Down,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            _ => Key::Other,
        },
        WKey::Character(c) => {
            if c.as_str() == " " { Key::Space } else { Key::Character(c.to_lowercase()) }
        }
        _ => Key::Other,
    }
}

impl<A: App> ApplicationHandler for Shell<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        if let Err(e) = self.create(event_loop) {
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gpu) = &mut self.gpu else { return };
        let scale = gpu.window.scale_factor() as f32;
        match event {
            WindowEvent::CloseRequested => {
                if self.ui.close_requested() {
                    event_loop.exit();
                } else {
                    self.after_input();
                }
            }
            WindowEvent::Moved(_) => {
                self.report_frame();
                self.after_input();
            }
            WindowEvent::Resized(size) => {
                gpu.config.width = size.width.max(1);
                gpu.config.height = size.height.max(1);
                gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                self.ui.resize(Size::new(size.width as f32 / scale, size.height as f32 / scale));
                self.ui.set_maximized(gpu.window.is_maximized());
                gpu.window.request_redraw();
                self.blur_strength = None;
                self.blur_attempts = 0;
                self.report_frame();
                self.sync_window();
            }
            WindowEvent::ScaleFactorChanged { .. } => gpu.window.request_redraw(),
            WindowEvent::Occluded(hidden) => {
                self.hidden = hidden;
                if !hidden {
                    self.retry_at = None;
                    gpu.window.request_redraw();
                }
            }
            WindowEvent::ThemeChanged(t) => {
                self.blur_strength = None;
                self.blur_attempts = 0;
                self.ui.set_system_scheme(scheme_of(t));
                self.after_input();
            }
            WindowEvent::Focused(f) => {
                self.blur_strength = None;
                self.blur_attempts = 0;
                self.dispatch(Event::WindowFocus(f));
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                let mods = self.modifiers();
                self.ui.set_modifiers(mods);
            }
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } if self.resizing.is_some() => {
                let Some(r) = &self.resizing else { return };
                let Ok(now) = gpu.window.outer_position() else { return };
                // The pointer's place on the screen, since the window moves under it.
                let (dx, dy) = (now.x as f64 + x - r.grab.0, now.y as f64 + y - r.grab.1);
                let min = self.settings.min_size.unwrap_or(Size::new(120.0, 80.0));
                let (min_w, min_h) = (min.w as f64 * scale as f64, min.h as f64 * scale as f64);
                let (mut left, mut top, mut right, mut bottom) = (r.origin.0, r.origin.1, r.origin.0 + r.size.0, r.origin.1 + r.size.1);
                use ResizeEdge::*;
                if matches!(r.edge, West | NorthWest | SouthWest) {
                    left = (left + dx).min(right - min_w);
                }
                if matches!(r.edge, East | NorthEast | SouthEast) {
                    right = (right + dx).max(left + min_w);
                }
                if matches!(r.edge, North | NorthWest | NorthEast) {
                    top = (top + dy).min(bottom - min_h);
                }
                if matches!(r.edge, South | SouthWest | SouthEast) {
                    bottom = (bottom + dy).max(top + min_h);
                }
                gpu.window.set_outer_position(PhysicalPosition::new(left.round(), top.round()));
                let _ = gpu.window.request_inner_size(winit::dpi::PhysicalSize::new((right - left).round().max(1.0), (bottom - top).round().max(1.0)));
            }
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                let p = Point::new(x as f32 / scale, y as f32 / scale);
                self.pointer = Some(p);
                self.dispatch(Event::PointerMoved { pos: p });
                if let (Some(edge), Some(gpu)) = (self.resize_edge(p), &self.gpu) {
                    gpu.window.set_cursor(cursor_icon(CursorIcon::Resize(edge)));
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.dispatch(Event::PointerLeft);
            }
            WindowEvent::MouseInput { state: ElementState::Released, .. } if self.resizing.is_some() => {
                self.resizing = None;
                self.report_frame();
                self.after_input();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(pos) = self.pointer else { return };
                let button = match button {
                    MouseButton::Left => PointerButton::Primary,
                    MouseButton::Right => PointerButton::Secondary,
                    MouseButton::Middle => PointerButton::Middle,
                    MouseButton::Back => PointerButton::Other(3),
                    MouseButton::Forward => PointerButton::Other(4),
                    MouseButton::Other(n) => PointerButton::Other(n),
                };
                if state == ElementState::Pressed && button == PointerButton::Primary
                    && let (Some(edge), Some(gpu)) = (self.resize_edge(pos), &self.gpu) {
                        let _ = gpu.window.drag_resize_window(resize_dir(edge));
                        return;
                    }
                self.dispatch(match state {
                    ElementState::Pressed => Event::PointerPressed { pos, button },
                    ElementState::Released => Event::PointerReleased { pos, button },
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(pos) = self.pointer else { return };
                let d = match delta {
                    MouseScrollDelta::LineDelta(x, y) => Point::new(-x * 48.0, -y * 48.0),
                    MouseScrollDelta::PixelDelta(p) => Point::new(-p.x as f32 / scale, -p.y as f32 / scale),
                };
                self.dispatch(Event::Wheel { pos, delta: d });
            }
            // One event arrives per file; they are handed on together once
            // the events for this drop have all come in.
            WindowEvent::DroppedFile(path) => self.dropped.push(path),
            WindowEvent::PinchGesture { delta, .. } => {
                let Some(pos) = self.pointer else { return };
                self.dispatch(Event::Pinch { pos, factor: (1.0 + delta as f32).max(0.1) });
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let modifiers = self.modifiers();
                let key = map_key(&event.logical_key);
                if modifiers.command() && key == Key::Character("v".into()) {
                    let text = self.clipboard.as_mut().and_then(|c| c.get_text().ok());
                    self.ui.set_clipboard(text);
                }
                self.dispatch(Event::Key(KeyEvent {
                    key,
                    pressed: event.state == ElementState::Pressed,
                    repeat: event.repeat,
                    modifiers,
                    text: event.text.map(|t| t.to_string()),
                }));
            }
            WindowEvent::Ime(winit::event::Ime::Commit(t)) => self.dispatch(Event::Ime(t)),
            _ => {}
        }
        if self.close {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.dropped.is_empty() {
            let paths = std::mem::take(&mut self.dropped);
            // No pointer events arrive during another app's drag, so ask.
            let pos = self.gpu.as_ref().and_then(|g| crate::platform::pointer_position(&g.window)).or(self.pointer).unwrap_or(Point::ZERO);
            self.pointer = Some(pos);
            self.dispatch(Event::FilesDropped { pos, paths });
        }
        let now = Instant::now();
        let mut wake = self.ui.tick(now);
        if self.retry_at.is_some_and(|t| t <= now) {
            self.retry_at = None;
        }
        if let Some(gpu) = &self.gpu
            && self.ui.needs_redraw()
            && self.can_draw() {
                gpu.window.request_redraw();
            }
        if let Some(t) = self.retry_at.filter(|_| !self.hidden) {
            wake = Some(wake.map_or(t, |w| w.min(t)));
        }
        self.sync_window();
        if self.close || self.ui.should_exit() {
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(match wake {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }
}
