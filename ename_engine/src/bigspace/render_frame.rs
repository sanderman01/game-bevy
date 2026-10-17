//! Converting absolute positions into the f32 render space big_space draws in, for code that
//! queries render-space data (`GlobalTransform`, avian positions) at an absolute point.

use bevy::{
    math::{DAffine3, DVec3},
    prelude::*,
};
use big_space::prelude::{BigSpace, Grid};

/// The smallest step a vertical ray moves off its point, in metres.
const MIN_STEP: f64 = 1e-4;

/// A root grid's render frame: its floating origin's cell and the transform from that cell to
/// render space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RenderFrame {
    origin: DVec3,
    to_render: DAffine3,
}

impl RenderFrame {
    /// The frame `grid` renders in, as of its last propagation.
    pub(crate) fn of(grid: &Grid) -> Self {
        let local = grid.local_floating_origin();
        Self {
            origin: grid.cell_to_float(&local.cell()),
            to_render: local.grid_transform(),
        }
    }

    /// The frame of the one root grid, or `None` with zero or several.
    pub(crate) fn of_single_root<'a>(
        mut roots: impl Iterator<Item = &'a Grid>,
    ) -> Option<RenderFrame> {
        match (roots.next(), roots.next()) {
            (Some(grid), None) => Some(Self::of(grid)),
            _ => None,
        }
    }

    pub(crate) fn render_point(&self, absolute: DVec3) -> Vec3 {
        self.to_render
            .transform_point3(absolute - self.origin)
            .as_vec3()
    }

    pub(crate) fn direction_to_render(&self, direction: DVec3) -> Option<Dir3> {
        Dir3::new(self.to_render.transform_vector3(direction).as_vec3()).ok()
    }

    /// A few f32 steps of the render-space Y of `point`, never less than 0.1 mm: the least a
    /// vertical ray must move to leave a surface the point lies on.
    pub(crate) fn y_step(&self, point: DVec3) -> f64 {
        let render = self.render_point(point);
        (f64::from(render.y.abs()) * f64::from(f32::EPSILON) * 4.0).max(MIN_STEP)
    }
}

/// The root grids a render frame is taken from.
pub(crate) type RootGrids<'w, 's> = Query<'w, 's, &'static Grid, With<BigSpace>>;
