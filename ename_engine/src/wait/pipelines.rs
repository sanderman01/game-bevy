//! The `pipelines` settle probe: render pipelines still compiling.
//!
//! The pipeline cache lives in the render world, so a render-world system stores its waiting count
//! after `RenderSystems::Render` into a counter both worlds hold. The main world reads it one
//! frame behind under pipelined rendering; `stable_frames` covers that. A pipeline whose shader
//! failed leaves the waiting set, so a broken shader does not block settling.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use bevy::{
    prelude::*,
    render::{Render, RenderApp, RenderSystems, render_resource::PipelineCache},
};

use super::{Activity, SettleAppExt};

/// Pipelines the render world's cache was still waiting on after its last render.
#[derive(Resource, Clone, Default)]
struct PipelinesWaiting(Arc<AtomicU32>);

pub(super) fn register(app: &mut App) {
    let waiting = PipelinesWaiting::default();
    app.insert_resource(waiting.clone())
        .register_settle_probe("pipelines", pipelines_probe);
    if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
        render_app
            .insert_resource(waiting)
            .add_systems(Render, store_waiting_pipelines.after(RenderSystems::Render));
    }
}

fn store_waiting_pipelines(cache: Option<Res<PipelineCache>>, waiting: Res<PipelinesWaiting>) {
    let Some(cache) = cache else {
        return;
    };
    let count = u32::try_from(cache.waiting_pipelines().count()).unwrap_or(u32::MAX);
    waiting.0.store(count, Ordering::Relaxed);
}

fn pipelines_probe(waiting: Res<PipelinesWaiting>) -> Activity {
    Activity::idle().pending(
        "pipelines_waiting",
        waiting.0.load(Ordering::Relaxed) as usize,
    )
}
