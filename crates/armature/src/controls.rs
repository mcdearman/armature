//! How controls behave, without how they look. Each type here handles the
//! input for one kind of control and keeps its state; a toolkit's widget
//! owns one, hands it events along with the geometry it draws at, and
//! paints from the state. A new look then costs only the painting.

mod field;
mod scroll;
mod slider;

pub use field::{byte_at, char_at, word_left, word_right, FieldAction, FieldLogic, FieldState};
pub use scroll::{ScrollLogic, ScrollState, ScrollStep};
pub use slider::{SliderChange, SliderLogic, SliderState};
