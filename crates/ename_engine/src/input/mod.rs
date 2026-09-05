//! Input: turns device input into camera and gameplay intent.
//!
//! Nothing in this module names a key. A layer above the engine maps devices to intent; the
//! engine consumes intent. That is what lets the editor and a future gameplay mode drive the
//! same camera without either knowing about the other.

mod fly_camera;

pub use fly_camera::{FlyCameraIntent, FlyCameraPlugin, FlyCameraSystems};
