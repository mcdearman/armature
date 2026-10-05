//! Platform window effects.

use winit::window::Window;

/// Starts a system drag of `paths` from the window, so they can be dropped
/// on other apps. Call it while the pointer event that began the drag is
/// being handled. Returns false where this is not supported yet: everywhere
/// but macOS.
pub(crate) fn drag_files(window: &Window, paths: &[std::path::PathBuf]) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::drag_files(window, paths)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, paths);
        false
    }
}

/// Where the pointer is in the window, in logical pixels, when the platform
/// can say without a pointer event. Needed while another app's drag is over
/// the window, because no pointer events arrive then.
pub(crate) fn pointer_position(window: &Window) -> Option<armature_render::Point> {
    #[cfg(target_os = "macos")]
    {
        macos::pointer_position(window)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        None
    }
}

/// Makes the window a plain rectangle with no system frame, or gives the
/// frame back. macOS rounds the corners of every titled window, so a bare,
/// see-through window drops its title bar to get square corners. Other
/// platforms' windows are already undecorated rectangles.
pub(crate) fn set_square(window: &Window, square: bool) {
    #[cfg(target_os = "macos")]
    macos::set_square(window, square);
    #[cfg(not(target_os = "macos"))]
    let _ = (window, square);
}

/// Sets the strength of the platform blur set up by [`set_blur`], in logical
/// pixels. Returns false when the platform's blur layers do not exist yet
/// (macOS builds them lazily), so the caller should try again after the next
/// frame. Platforms without a strength control return true.
pub(crate) fn set_blur_strength(window: &Window, radius: f32) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::set_blur_strength(window, radius)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, radius);
        true
    }
}

/// Turns the compositor's background blur behind a transparent window on or
/// off. `radius` is the corner radius the app draws the window with, so the blur
/// does not show outside the rounded corners.
///
/// - macOS 26+: clear Liquid Glass clipped to `radius`. Older macOS: a
///   behind-window `NSVisualEffectView` with the HUD material.
/// - Windows 10/11: acrylic via DWM. Windows 11 rounds the blur with its own
///   fixed corner, so `radius` is not applied.
/// - Linux, Wayland: KDE's blur protocol, over the whole surface for now.
///   Other compositors show the window translucent without blur.
/// - Linux, X11: translucent only.
pub(crate) fn set_blur(window: &Window, enabled: bool, radius: f32) {
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{
            apply_liquid_glass, apply_vibrancy, clear_liquid_glass, clear_vibrancy, LiquidGlassOptions, NSGlassEffectViewStyle, NSVisualEffectMaterial,
            NSVisualEffectState,
        };
        // Replacing the view is the only way to change its radius.
        let _ = clear_liquid_glass(window);
        let _ = clear_vibrancy(window);
        if enabled {
            let radius = radius as f64;
            // Clear Liquid Glass (macOS 26+) is the most transparent system
            // blur. The app paints its own tint on top, so none is set here.
            let result = apply_liquid_glass(window, LiquidGlassOptions::new(NSGlassEffectViewStyle::Clear).radius(radius)).or_else(|_| {
                // Older macOS: the HUD material is the least tinted blur.
                apply_vibrancy(window, NSVisualEffectMaterial::HudWindow, Some(NSVisualEffectState::Active), Some(radius))
            });
            match result {
                Ok(()) => macos::raise_metal_layer(window),
                Err(e) => eprintln!("armature: window blur unavailable: {e}"),
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = radius;
        let result = if enabled {
            window_vibrancy::apply_acrylic(window, Some((0, 0, 0, 0)))
        } else {
            window_vibrancy::clear_acrylic(window)
        };
        if let Err(e) = result {
            eprintln!("armature: window blur unavailable: {e}");
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = radius;
        window.set_blur(enabled);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::Window;

    /// wgpu draws into a `CAMetalLayer` added directly to the content view's
    /// layer, and window-vibrancy inserts its blur view into the same view.
    /// AppKit attaches the blur view's layer lazily and places it above the
    /// Metal layer, hiding the app's content. Raising the Metal layer's
    /// z-position keeps it on top whatever order AppKit chooses.
    /// wgpu draws into a `CAMetalLayer` added directly to the content view's
    /// layer, and window-vibrancy inserts its blur view into the same view.
    /// AppKit attaches the blur view's layer lazily and places it above the
    /// Metal layer, hiding the app's content. Raising the Metal layer's
    /// z-position keeps it on top whatever order AppKit chooses.
    fn ns_string(s: &std::ffi::CStr) -> *mut AnyObject {
        let cls = objc2::runtime::AnyClass::get(c"NSString").expect("Foundation is loaded");
        unsafe { msg_send![cls, stringWithUTF8String: s.as_ptr()] }
    }

    /// Sets the blur radius on the system glass. Neither Liquid Glass nor
    /// NSVisualEffectView exposes this publicly; both are backed by a
    /// `CABackdropLayer` whose filter has a radius input:
    /// `glassBackground.inputBlurRadius` (Liquid Glass, default 10) or
    /// `gaussianBlur.inputRadius` (visual effect views). If Apple changes
    /// either, this finds nothing and the system default stays.
    pub(super) fn set_blur_strength(window: &Window, radius: f32) -> bool {
        unsafe fn visit(layer: *mut AnyObject, radius: f64, found: &mut bool) {
            unsafe {
                let filters: *mut AnyObject = msg_send![layer, filters];
                let n: usize = if filters.is_null() { 0 } else { msg_send![filters, count] };
                for i in 0..n {
                    let f: *mut AnyObject = msg_send![filters, objectAtIndex: i];
                    let name: *mut AnyObject = msg_send![f, name];
                    if name.is_null() {
                        continue;
                    }
                    let name_c: *const std::ffi::c_char = msg_send![name, UTF8String];
                    let key = match std::ffi::CStr::from_ptr(name_c).to_bytes() {
                        b"glassBackground" => c"inputBlurRadius",
                        b"gaussianBlur" => c"inputRadius",
                        _ => continue,
                    };
                    let path = format!("filters.{}.{}", std::ffi::CStr::from_ptr(name_c).to_string_lossy(), key.to_string_lossy());
                    let path = std::ffi::CString::new(path).expect("no interior nul");
                    let number_cls = objc2::runtime::AnyClass::get(c"NSNumber").expect("Foundation is loaded");
                    let value: *mut AnyObject = msg_send![number_cls, numberWithDouble: radius];
                    let _: () = msg_send![layer, setValue: value, forKeyPath: ns_string(&path)];
                    *found = true;
                }
                let subs: *mut AnyObject = msg_send![layer, sublayers];
                let n: usize = if subs.is_null() { 0 } else { msg_send![subs, count] };
                for i in 0..n {
                    let l: *mut AnyObject = msg_send![subs, objectAtIndex: i];
                    visit(l, radius, found);
                }
            }
        }
        let Ok(handle) = window.window_handle() else { return true };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return true };
        let mut found = false;
        // SAFETY: winit hands out a valid NSView and this runs on the main thread.
        unsafe {
            let view = h.ns_view.as_ptr().cast::<AnyObject>();
            let root: *mut AnyObject = msg_send![view, layer];
            if !root.is_null() {
                visit(root, radius.max(0.0) as f64, &mut found);
            }
        }
        found
    }

    /// Swaps the titled style, whose corners the system rounds, for a
    /// borderless one. winit's `set_decorations` would also drop the
    /// full-size content view a custom title bar relies on, so the style mask
    /// is set here.
    pub(super) fn set_square(window: &Window, square: bool) {
        const TITLED: usize = 1 << 0;
        const CLOSABLE: usize = 1 << 1;
        const MINIATURIZABLE: usize = 1 << 2;
        const RESIZABLE: usize = 1 << 3;
        const FULL_SIZE_CONTENT: usize = 1 << 15;
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
        // SAFETY: winit hands out a valid NSView and this runs on the main thread.
        unsafe {
            let view = h.ns_view.as_ptr().cast::<AnyObject>();
            let ns_window: *mut AnyObject = msg_send![view, window];
            if ns_window.is_null() {
                return;
            }
            let old: usize = msg_send![ns_window, styleMask];
            let frame = TITLED | CLOSABLE | MINIATURIZABLE | FULL_SIZE_CONTENT;
            let new = if square { old & !frame } else { old | frame };
            let new = if window.is_resizable() { new | RESIZABLE } else { new };
            if new == old {
                return;
            }
            let _: () = msg_send![ns_window, setStyleMask: new];
            if !square {
                // The title bar buttons come back with the frame; the app draws its own.
                for button in 0..3usize {
                    let b: *mut AnyObject = msg_send![ns_window, standardWindowButton: button];
                    if !b.is_null() {
                        let _: () = msg_send![b, setHidden: true];
                    }
                }
            }
            // Changing the style mask drops keyboard focus from the view.
            let _: bool = msg_send![ns_window, makeFirstResponder: view];
        }
    }

    unsafe extern "C" {
        fn armature_drag_files(ns_view: *mut std::ffi::c_void, paths: *const *const std::ffi::c_char, count: std::ffi::c_int) -> std::ffi::c_int;
        fn armature_pointer_in_view(ns_view: *mut std::ffi::c_void, x: *mut f64, y: *mut f64) -> std::ffi::c_int;
    }

    pub(super) fn drag_files(window: &Window, paths: &[std::path::PathBuf]) -> bool {
        let Ok(handle) = window.window_handle() else { return false };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
        let c_paths: Vec<std::ffi::CString> = paths.iter().filter_map(|p| std::ffi::CString::new(p.to_string_lossy().as_bytes()).ok()).collect();
        let pointers: Vec<*const std::ffi::c_char> = c_paths.iter().map(|p| p.as_ptr()).collect();
        // SAFETY: winit hands out a valid NSView, this runs on the main thread,
        // and the path strings outlive the call.
        unsafe { armature_drag_files(h.ns_view.as_ptr(), pointers.as_ptr(), pointers.len() as std::ffi::c_int) != 0 }
    }

    pub(super) fn pointer_position(window: &Window) -> Option<armature_render::Point> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
        let (mut x, mut y) = (0.0, 0.0);
        // SAFETY: winit hands out a valid NSView and the out-pointers are valid.
        (unsafe { armature_pointer_in_view(h.ns_view.as_ptr(), &mut x, &mut y) } != 0).then(|| armature_render::Point::new(x as f32, y as f32))
    }

    pub(super) fn raise_metal_layer(window: &Window) {
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
        // SAFETY: winit hands out a valid NSView, and this runs on the main
        // thread (window events and sync_window only run there).
        unsafe {
            let view = h.ns_view.as_ptr().cast::<AnyObject>();
            let root: *mut AnyObject = msg_send![view, layer];
            if root.is_null() {
                return;
            }
            let subs: *mut AnyObject = msg_send![root, sublayers];
            let n: usize = if subs.is_null() { 0 } else { msg_send![subs, count] };
            for i in 0..n {
                let layer: *mut AnyObject = msg_send![subs, objectAtIndex: i];
                if (*layer).class().name().to_bytes().ends_with(b"MetalLayer") {
                    let _: () = msg_send![layer, setZPosition: 1.0f64];
                }
            }
        }
    }
}
