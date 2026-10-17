# Issue: Scene View should render offscreen, like Game View

## Status: WONTFIX (for now)

The airshipgame workaround stays. Neither fix is being pursued now:

- The offscreen move also needs a change to Bevy's transform gizmo, either upstream or a vendored
  copy (see [Gizmo](#gizmo)). That makes it at least as costly as fixing Bevy's depth pyramid.
- We don't want to upstream the issue right now.
- Bevy 0.20.0 has the same bug and the same gizmo limits (see [Bevy 0.20](#bevy-020)), so
  upgrading changes nothing.

Reopen this when one of these happens: Bevy fixes the viewport offset; we decide to patch
Bevy; or Scene View GPU timings or occlusion culling become necessary.

## Problem

The Scene View's `EditorCamera` draws into the window and is cropped to the dock tab with
`camera.viewport` (`ename_editor/src/viewport.rs`). Bevy 0.19.1's GPU occlusion culling
breaks on a camera that has a viewport. It wrongly culls visible meshes, so whole mesh batches
disappear in the Scene View.

Repro (airshipgame): the Scene View, about 2.5 km up over forested terrain, showed
square terrain tiles with no land cover. The camera was still. Removing `OcclusionCulling`
from the Editor Camera filled the tiles; adding it back emptied them again.

## Cause (Bevy bug)

- Bevy sizes a camera's depth texture to the whole render target (the window), not to the
  viewport.
- `downsample_depth.wgsl` `load_mip_0` builds the occlusion depth pyramid from the whole
  depth texture, starting at (0,0), with no viewport offset.
- `mesh_preprocess.wgsl` maps each mesh's bounds to UVs in viewport coordinates.
- With a viewport such as 1535×990 at (385,46) in a 2560×1382 window, the culling test samples
  the wrong region of the pyramid. A visible mesh is culled wherever that region holds depth
  nearer than the mesh.

Evidence in the 0.19.1 source:

- `prepare_view_depth_pyramids` (`bevy_core_pipeline/src/mip_generation/experimental/depth.rs`)
  sizes the pyramid from `view.viewport.zw()`, the viewport's size, so Bevy does mean to support
  viewports.
- `load_mip_0` (`downsample_depth.wgsl`) fills it from `textureDimensions(mip_0)`, the whole
  depth texture, starting at (0,0). It never applies `viewport.xy`.
- `occlusion_culling.wgsl` `get_occluder_depth` indexes the pyramid with the mesh's viewport UV.

The `OcclusionCulling` docs list `DepthPrepass` as required and deferred shading as
incompatible. They say nothing against viewports. Their only warning is a general one: the
experimental culling can hide meshes that should be visible. No GitHub issue about the viewport
offset was found (2026-10-17).

Not yet done: a Bevy-only repro, such as the `split_screen` example with `DepthPrepass` and
`OcclusionCulling` on its cameras. Write one before reporting upstream.

## Bevy 0.20

Compared with the 0.20.0 source: nothing fixes this issue or makes either fix easier.

- Culling: the depth texture is still sized to `physical_target_size` (`core_3d/mod.rs`), the
  pyramid to `view.viewport.zw()`, and `load_mip_0` (now `downsample_depth.wesl`) still reads
  from (0,0) with no offset. `mesh_preprocess.wesl` still maps bounds to viewport UVs via
  `ndc_to_uv`. The changes in these files don't touch the bug: WGSL became WESL,
  `ViewDepthTexture` became `ViewDepthStencilTexture`, and timing diagnostics were added.
- Gizmo: it still reads `window.cursor_position()`, the overlay camera still copies only the
  viewport, and `GizmoOverlayCamera` is still private. The only changes are a view-axis scale
  handle and `scale_sensitivity`.

## Current workaround (airshipgame, not the engine)

`airshipgame-lib/src/atmosphere.rs` `cull_occluded_on_full_targets` runs in `Last`, after the
editor sets viewports in `PostUpdate`. It removes `OcclusionCulling` from any camera with a
viewport and keeps it on cameras without one. The Scene View therefore runs without occlusion
culling. GPU timings differ between the views, so they must be taken in the Game View.

## Proposal

Render the Scene View to an editor-owned image, as `game_view.rs` already does for the Game View,
and paint it in the tab with egui. This would:

- give the Scene View working occlusion culling again;
- make GPU timings comparable between Scene View and Game View;
- let screenshots of the Scene View capture the image instead of a crop of the window;
- remove the egui composite constraint described in `game_view.rs:4-11` for the editor camera
  too, so the Editor Camera could carry `Hdr` or other settings freely. This holds for the gizmo
  overlay only if the editor mirrors those settings onto its camera (see below).

## Why it is a window viewport today

No recorded reason. History suggests it was the original single-camera setup (6ac8a81, the
usual bevy_egui dock pattern). The Scene View tab was built on top of it (ae1c1cd). The Game
View moved offscreen only to fix the HDR compositing bug (5de4021), and the Scene View was left
as it was.

## What would need to change

### Gizmo

Bevy's transform gizmo (`bevy_gizmos_render/src/transform_gizmo_render.rs`) spawns its own
overlay camera: `order: 1`, render layer 15, target left at the default (the primary window).
Each frame `update_gizmo_meshes` (in `TransformGizmoRenderStep`) copies only the transform and
the `viewport` from the `TransformGizmoCamera`. It never copies the target.

- **Render target:** the editor must point the overlay camera at the Scene View image itself.
  Its marker `GizmoOverlayCamera` is private, so the only way to find it is an `Added<Camera>`
  query for render layer 15, which is fragile. The target needs setting only once, because Bevy
  never overwrites it, and the copied viewport becomes `None`, which is correct. The camera
  must also match the Editor Camera's `Hdr`, MSAA and format. Otherwise it gets a different
  view target, and the composite bug from `game_view.rs:4-11` returns inside the image.
- **Cursor input (the blocker):** hover and drag (`bevy_gizmos/src/transform_gizmo.rs`)
  pass `window.cursor_position()` to `camera.viewport_to_world`, which subtracts
  `logical_viewport_rect().min`. On an image target that min is (0,0), so hit-testing is offset
  by the tab's top-left corner. The gizmo can't be given another cursor position: it has no
  input hook; setting the window's cursor moves the OS cursor; and keeping a viewport on the
  image brings the depth-pyramid bug back. Fix it upstream (let the gizmo take its cursor from
  `bevy_picking`'s `PointerLocation` or a resource) or vendor the hover and drag systems. Set the
  image's `scale_factor` to window scale × egui zoom so logical units match.

### Other

- **Picking:** click-to-select uses `bevy_picking` with window pointer positions. Drive it
  with a custom pointer on the image target, whose location is the cursor mapped into the image.
  `gizmo_should_run` and `pointer_in_viewport` stay as they are.
- **Grid and other gizmos:** probably nothing to change. Immediate-mode `Gizmos` (selection,
  physics, big_space grids) draw through every camera whose render layers match their config,
  so they follow the Editor Camera to the image. Check at runtime.
- **Screenshots:** the `editor_camera` capture currently crops the window; switch it to the
  image.
- **Resize:** reuse `resize_game_view`'s logic for the tab's size and scale factor.

## Alternative

Fix Bevy so the depth pyramid honours the viewport offset: pass `viewport.xy` to the downsample
shader and add it to `st` in `load_mip_0`, and check that the virtual-size math uses the
viewport size, not the texture size. That helps every viewport camera. Until it lands upstream,
we could carry it on 0.19.1 with `[patch.crates-io]` for `bevy_core_pipeline`; it should port
to 0.20 almost unchanged. This is the smaller change of the two.
