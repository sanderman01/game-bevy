//! `ename_editor` -- the egui editor: dock layout, selection, inspector panels, gizmo integration.
//!
//! Sits above `ename_engine` and links into a target only when that target asks for it. A
//! shipping build does not contain this crate. See `docs/design.md`.
//!
//! Based on the example at:
//! <https://github.com/jakobhellermann/bevy-inspector-egui/blob/main/crates/bevy-inspector-egui/examples/integrations/egui_dock.rs>.

mod camera;
pub mod editor;
mod gizmo;

pub use crate::editor::EditorPlugins;
