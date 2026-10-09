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

/// Set when the app is asked to open again while running, for the event
/// loop to pass on.
#[cfg(target_os = "macos")]
static REOPENED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Wakes the event loop to hear of it.
#[cfg(target_os = "macos")]
static REOPEN_WAKE: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

/// Hears of every entry chosen from a menu, by its ID.
static OTHER_MENUS: std::sync::OnceLock<MenuHeard> = std::sync::OnceLock::new();
type MenuHeard = Box<dyn Fn(&str) + Send + Sync>;

/// Has `heard` called, with the entry's ID, whenever an entry is chosen
/// from any menu made with `muda` in this program: the app's own, and any
/// other, such as a tray icon's.
///
/// `muda` lets one handler be set for the whole program, and on macOS
/// this framework sets it, for the menu bar; so `MenuEvent::set_event_handler`
/// called anywhere else is ignored there, without a word. A tray icon's
/// menu is listened to through this instead. Only the first call counts.
/// Where the framework sets no handler (everywhere but macOS, so far) it
/// is never called, and `muda`'s own way works.
pub fn on_menu_chosen(heard: impl Fn(&str) + Send + Sync + 'static) {
    let _ = OTHER_MENUS.set(Box::new(heard));
}

/// Opens a window and runs `app` until it closes.
pub fn run<A: App>(app: A) -> Result<(), Error> {
    let event_loop = EventLoop::new().map_err(Error::EventLoop)?;
    let settings = app.window();
    let mut ui = Ui::new(app, settings.size, Scheme::Light);
    let waker = event_loop.create_proxy();
    ui.start(Arc::new(move || {
        let _ = waker.send_event(());
    }));
    #[cfg(target_os = "macos")]
    {
        let waker = event_loop.create_proxy();
        let _ = REOPEN_WAKE.set(Box::new(move || {
            let _ = waker.send_event(());
        }));
    }
    #[cfg(target_os = "macos")]
    let menu_events = {
        // The system's menu bar reports a chosen entry by its ID.
        ui.set_native_menus(true);
        let (tx, rx) = std::sync::mpsc::channel();
        let waker = event_loop.create_proxy();
        muda::MenuEvent::set_event_handler(Some(move |e: muda::MenuEvent| {
            // A menu that is not the app's own, such as a tray icon's, is the app's to answer.
            if let Some(other) = OTHER_MENUS.get() {
                other(&e.id().0);
            }
            let _ = tx.send(e.id().0.clone());
            let _ = waker.send_event(());
        }));
        rx
    };
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
        captured: false,
        hidden: false,
        retry_at: None,
        state: None,
        resizing: None,
        dropped: vec![],
        #[cfg(target_os = "macos")]
        menu: None,
        #[cfg(target_os = "macos")]
        menu_events,
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
    /// The pointer is held in the window and hidden: see
    /// [`WindowRequest::CapturePointer`].
    captured: bool,
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
    /// The system menu bar as last built, and what it was built from.
    #[cfg(target_os = "macos")]
    menu: Option<(u64, muda::Menu)>,
    #[cfg(target_os = "macos")]
    menu_events: std::sync::mpsc::Receiver<String>,
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
            // With whatever the app wants of what this adapter has.
            let app = self.ui.app();
            let descriptor = wgpu::DeviceDescriptor { required_features: app.wanted_features(adapter.features()) & adapter.features(), required_limits: app.wanted_limits(&adapter.limits()), ..Default::default() };
            let (device, queue) = adapter.request_device(&descriptor).await.map_err(|e| Error::Graphics(e.to_string()))?;
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
        self.ui.set_scale(scale);
        // Before the first view: an app that draws with the device has it by then.
        self.ui.graphics(&crate::app::Graphics::of(&device, &queue));
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
                    // Windows shown from a global shortcut should come to the
                    // front; ones that appear on their own should not take over.
                    if !state.passive {
                        gpu.window.focus_window();
                    }
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

    /// Puts the app's menus in the system's menu bar, rebuilding it only
    /// when they have changed, and carries out entries chosen there.
    #[cfg(target_os = "macos")]
    fn sync_menus(&mut self) {
        use muda::{MenuItem, PredefinedMenuItem, Submenu};
        for id in self.menu_events.try_iter().collect::<Vec<_>>() {
            let Some(rest) = id.strip_prefix("armature:") else { continue };
            let mut parts = rest.split(':');
            match (parts.next(), parts.next().map(str::parse::<usize>)) {
                (Some("app"), Some(Ok(entry))) => self.ui.choose_app_menu(entry),
                (Some(menu), Some(Ok(entry))) => {
                    if let Ok(menu) = menu.parse() {
                        self.ui.choose_menu(menu, entry);
                    }
                }
                _ => {}
            }
        }
        let menus = self.ui.menus();
        let own = self.ui.app_menu();
        // The app's own entries count as a menu when checking for changes.
        let signature = crate::menu::signature(&menus) ^ crate::menu::signature(&[crate::Menu { title: "app".into(), entries: own.clone() }]).rotate_left(1);
        if self.menu.as_ref().is_some_and(|(s, _)| *s == signature) {
            return;
        }
        let bar = muda::Menu::new();
        // The first menu is the app's own; macOS titles it with the app's name.
        let app = Submenu::new("App", true);
        let entry_item = |id: String, entry: &crate::MenuEntry<A::Message>| {
            let keys = entry.shortcut.as_ref().and_then(|s| s.accelerator().parse().ok());
            MenuItem::with_id(id, &entry.label, entry.message.is_some(), keys)
        };
        let _ = app.append_items(&[&PredefinedMenuItem::about(None, None), &PredefinedMenuItem::separator()]);
        // Settings and the like go here, under the app's name.
        for (e, item) in own.iter().enumerate() {
            let _ = if item.separator { app.append(&PredefinedMenuItem::separator()) } else { app.append(&entry_item(format!("armature:app:{e}"), item)) };
        }
        if !own.is_empty() {
            let _ = app.append(&PredefinedMenuItem::separator());
        }
        let _ = app.append_items(&[
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ]);
        let _ = bar.append(&app);
        for (m, menu) in menus.iter().enumerate() {
            let sub = Submenu::new(&menu.title, true);
            for (e, entry) in menu.entries.iter().enumerate() {
                let _ = if entry.separator {
                    sub.append(&PredefinedMenuItem::separator())
                } else {
                    sub.append(&entry_item(format!("armature:{m}:{e}"), entry))
                };
            }
            let _ = bar.append(&sub);
        }
        bar.init_for_nsapp();
        self.menu = Some((signature, bar));
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
        self.ui.set_scale(scale);
        // The app's own frame first, so that what is shown is this one's.
        let now = Instant::now();
        self.ui.step(now);
        let scene = self.ui.draw(gpu.renderer.text(), now);
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
                WindowRequest::CapturePointer(hold) => {
                    use winit::window::CursorGrabMode;
                    // Held where it is, or failing that kept inside the window; either way out of sight.
                    let held = hold && (gpu.window.set_cursor_grab(CursorGrabMode::Locked).is_ok() || gpu.window.set_cursor_grab(CursorGrabMode::Confined).is_ok());
                    if !held {
                        let _ = gpu.window.set_cursor_grab(CursorGrabMode::None);
                    }
                    gpu.window.set_cursor_visible(!held);
                    self.captured = held;
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
        CursorIcon::Crosshair => W::Crosshair,
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
            NamedKey::F1 => Key::F(1),
            NamedKey::F2 => Key::F(2),
            NamedKey::F3 => Key::F(3),
            NamedKey::F4 => Key::F(4),
            NamedKey::F5 => Key::F(5),
            NamedKey::F6 => Key::F(6),
            NamedKey::F7 => Key::F(7),
            NamedKey::F8 => Key::F(8),
            NamedKey::F9 => Key::F(9),
            NamedKey::F10 => Key::F(10),
            NamedKey::F11 => Key::F(11),
            NamedKey::F12 => Key::F(12),
            _ => Key::Other,
        },
        WKey::Character(c) => {
            if c.as_str() == " " { Key::Space } else { Key::Character(c.to_lowercase()) }
        }
        _ => Key::Other,
    }
}

impl<A: App> ApplicationHandler for Shell<A> {
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.ui.exiting();
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        // Launched: from here on, a click on the Dock icon is heard.
        #[cfg(target_os = "macos")]
        crate::platform::watch_reopen(Box::new(|| {
            REOPENED.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(wake) = REOPEN_WAKE.get() {
                wake();
            }
        }));
        if let Err(e) = self.create(event_loop) {
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: winit::event::DeviceId, event: winit::event::DeviceEvent) {
        // The pointer's own movement, which is all there is of it while it is held.
        if let (true, winit::event::DeviceEvent::MouseMotion { delta }) = (self.captured, event) {
            self.dispatch(Event::PointerMotion { delta: Point::new(delta.0 as f32, delta.1 as f32) });
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
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.ui.set_scale(scale_factor as f32);
                gpu.window.request_redraw();
            }
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
                // The pointer is not kept from someone who has gone elsewhere.
                if !f && self.captured {
                    let _ = gpu.window.set_cursor_grab(winit::window::CursorGrabMode::None);
                    gpu.window.set_cursor_visible(true);
                    self.captured = false;
                    self.dispatch(Event::PointerCaptureLost);
                }
                self.dispatch(Event::WindowFocus(f));
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                let mods = self.modifiers();
                self.ui.set_modifiers(mods);
                // Widgets hear of it as the pointer arriving where it
                // already is, with the new keys held.
                if let Some(pos) = self.ui.pointer() {
                    self.dispatch(Event::PointerMoved { pos });
                }
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
        // Before the redraw check below, so a chosen entry shows at once.
        #[cfg(target_os = "macos")]
        if self.gpu.is_some() {
            self.sync_menus();
        }
        #[cfg(target_os = "macos")]
        if REOPENED.swap(false, std::sync::atomic::Ordering::Relaxed) {
            self.ui.reopened();
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
