# Armature

A cross-platform GUI framework in Rust with no look of its own. Armature opens windows, routes input, lays out and draws a tree of widgets, and keeps widget state between rebuilds. What things look like is up to a toolkit built on top.

[Neo](https://github.com/mcdearman/neo) is the first such toolkit. `crates/armature/examples/retro.rs` is a second, much smaller one, there to show that nothing in the framework assumes a style.

## Crates

| Crate | Role |
|---|---|
| `armature-render` | wgpu renderer. The caller supplies every colour, shadow and typeface. Each shape is one instanced SDF quad: rounded rectangles, borders, Gaussian drop and inner shadows, arcs, lines and area fills. Also backdrop blur, images with mipmaps, text, and PNG readback. |
| `armature` | Windows, input, layout, the app loop, widget state, a headless test harness, the widgets that only arrange or capture (rows, columns, stacks, spacing, pictures, mouse areas, plain labels), and the text-editing model. |

## How an app is written

Interfaces follow the Elm architecture. An `App` owns its state, turns messages into state changes in `update`, and describes the interface for the current state in `view`.

```rust
use armature::widgets::column;
use armature::{App, Element};

#[derive(Default)]
struct Hello;

impl App for Hello {
    type Message = ();
    fn update(&mut self, _: ()) {}
    fn view(&self) -> Element<()> {
        column().padding(24.0).push("Hello").into()
    }
}

fn main() -> Result<(), armature::Error> {
    armature::run(Hello)
}
```

## How a toolkit supplies its look

Three methods on `App` are the whole seam:

- `style` returns a `Style`. A toolkit attaches its own theme with `Style::new(theme)`, and its widgets read it back with `cx.style::<Theme>()`. The framework itself reads only the default text colour and style, the window's corner radius, and whether to blur what is behind the window.
- `fonts` returns the typefaces to draw text with. System fonts are always available as a fallback.
- `frame` wraps the view in whatever the window needs drawn: its background and, with `Decorations::Custom`, a title bar.

A toolkit usually hides these behind its own app trait, as Neo does, so that apps never see them.

## Running the example and the tests

```sh
cargo run -p armature --example retro
cargo test --workspace
```

The tests render on the GPU, headlessly, so they need a graphics adapter: any Metal, Vulkan, DX12 or GL device, including a software rasteriser.

## Platforms

macOS, Windows and Linux (Wayland and X11), through winit and wgpu.

## Licence

MIT or Apache-2.0, at your option.
