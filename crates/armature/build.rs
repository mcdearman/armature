//! Compiles the small Objective-C file behind dragging files out of a
//! window on macOS.

fn main() {
    println!("cargo:rerun-if-changed=src/mac_drag.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    cc::Build::new().file("src/mac_drag.m").flag("-fobjc-arc").compile("armature_mac_drag");
    println!("cargo:rustc-link-lib=framework=AppKit");
}
