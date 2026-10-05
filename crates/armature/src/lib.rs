//! # Armature
//!
//! A cross-platform GUI framework with no look of its own. It opens windows,
//! routes input, lays out and draws a tree of widgets, and keeps widget
//! state such as hover, focus and animation between rebuilds. What things
//! look like is up to a toolkit built on top: the framework carries the
//! toolkit's theme to its widgets as an opaque [`Style`] and never reads it.
//!
//! Interfaces are described with the Elm architecture: an [`App`] owns its
//! state, turns messages into state changes in `update`, and describes the
//! interface in `view`.
//!
//! Included here are the widgets that only arrange or capture, such as rows,
//! columns, stacks and mouse areas, and the logic of controls that are hard
//! to get right, such as the text-editing [`document`] model, without any
//! painting.

mod anim;
mod app;
mod core;
pub mod document;
mod event;
mod platform;
mod runtime;
mod shell;
pub mod testing;
pub mod widgets;

pub use anim::Anim;
pub use app::{App, Chrome, Decorations, Proxy, Scheme, Style, Subscription, WindowGeometry, WindowSettings, WindowState};
pub use core::{Align, Cx, CursorIcon, DrawCx, Element, EventCx, Length, Limits, Padding, ResizeEdge, Widget, WidgetId, WindowRequest};
pub use event::{Event, Key, KeyEvent, Modifiers, PointerButton, Status};
pub use runtime::Ui;
pub use shell::{run, Error};

pub use armature_render::{Color, Corners, FontFamily, Fonts, Image, Paint, Point, Rect, Scene, Shadow, Size, TextLayout, TextStyle};
