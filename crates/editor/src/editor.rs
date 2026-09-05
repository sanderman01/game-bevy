use bevy::camera::{Viewport, visibility::RenderLayers};
use bevy::reflect::TypeRegistry;
use bevy::window::{PrimaryWindow, Window};
use bevy::{
    app::{PluginGroupBuilder, TransformGizmoRenderStep},
    asset::{ReflectAsset, UntypedAssetId},
    color::palettes::tailwind::*,
    gizmos::transform_gizmo::TransformGizmoMeshMarker,
    picking::{
        Pickable,
        pointer::{PointerAction, PointerInput, PointerInteraction},
    },
    prelude::*,
};
use bevy_egui::{
    EguiGlobalSettings, EguiPostUpdateSet, EguiPrimaryContextPass, EguiZoomFactor,
    PrimaryEguiContext,
};
use bevy_inspector_egui::DefaultInspectorConfigPlugin;
use bevy_inspector_egui::bevy_egui::EguiContext;
use bevy_inspector_egui::bevy_inspector::hierarchy::{SelectedEntities, hierarchy_ui};
use bevy_inspector_egui::bevy_inspector::{
    self, ui_for_entities_shared_components, ui_for_entity_with_children,
};
use egui::{LayerId, UiBuilder};
use egui_dock::{DockArea, DockState, NodeIndex, Style};
use std::any::TypeId;

/// Marker for the camera that renders the editor's Game View.
///
/// The editor needs to tell its own 3D camera apart from the egui camera and from the
/// overlay camera the transform gizmo renderer spawns, and it cannot use
/// `engine::camera::MainCamera` because `engine` depends on `editor` and not the other
/// way around. Attach this to the main 3D camera when building a scene.
#[derive(Component, Debug, Default, Reflect)]
#[reflect(Component, Default)]
#[require(TransformGizmoCamera)]
pub struct EditorCamera;

pub struct EditorPluginGroup;

impl PluginGroup for EditorPluginGroup {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>().add(EditorPlugin)
    }
}

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MeshPickingPlugin)
            .add_plugins(TransformGizmoPlugin)
            // .add_plugins(bevy_framepace::FramepacePlugin) // reduces input lag
            .add_plugins(bevy_egui::EguiPlugin::default())
            .add_plugins(DefaultInspectorConfigPlugin)
            .insert_resource(UiState::new())
            // The gizmo reads the raw window cursor, so it must stand down while the
            // pointer is over a panel rather than the Game View.
            .configure_sets(PostUpdate, TransformGizmoSystems.run_if(gizmo_should_run))
            .add_systems(Startup, setup)
            .add_systems(EguiPrimaryContextPass, show_ui_system)
            // The dock rect is only known once the Egui pass has run, and the gizmo's
            // overlay camera copies the viewport during the render step, so a resize
            // reaches the handles in the same frame.
            .add_systems(
                PostUpdate,
                set_camera_viewport
                    .after(EguiPostUpdateSet::EndPass)
                    .before(TransformGizmoRenderStep),
            )
            .add_systems(
                Update,
                (
                    draw_mesh_intersections,
                    ignore_gizmo_mesh_picking,
                    gizmo_keyboard_shortcuts,
                ),
            )
            // The guard against selecting through a handle reads state that
            // `transform_gizmo_hover` writes this frame.
            .add_systems(PostUpdate, handle_pick_events.after(TransformGizmoSystems))
            .add_systems(
                PostUpdate,
                sync_gizmo_focus
                    .after(handle_pick_events)
                    .after(EguiPostUpdateSet::EndPass),
            )
            .register_type::<EditorCamera>()
            .register_type::<Option<Handle<Image>>>()
            .register_type::<AlphaMode>();
    }
}

/// The gizmo takes `window.cursor_position()` directly and knows nothing about the egui
/// panels covering part of the window, so it only runs while the pointer is over the Game
/// View. A drag already in progress keeps running wherever the cursor goes: cutting it off
/// mid-drag would strand `TransformGizmoState::active` and leave the cursor confined.
fn gizmo_should_run(ui_state: Res<UiState>, gizmo: Res<TransformGizmoState>) -> bool {
    ui_state.pointer_in_viewport || gizmo.active
}

/// The gizmo renders through an always-on-top overlay camera on its own render layer, and
/// the mesh picking backend builds a ray for that camera too. Without this, clicking a
/// handle selects the handle's mesh entity and paints a debug sphere on it.
fn ignore_gizmo_mesh_picking(
    handles: Query<Entity, Added<TransformGizmoMeshMarker>>,
    mut commands: Commands,
) {
    for entity in &handles {
        commands.entity(entity).insert(Pickable::IGNORE);
    }
}

/// The gizmo reads no keyboard input by design, so the editor picks the bindings: W, E and
/// R select the mode, X toggles between world and local space. The camera fly shares W and
/// E but only while the right mouse button is held, so the shortcuts stand down for it.
fn gizmo_keyboard_shortcuts(
    ui_state: Res<UiState>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut settings: ResMut<TransformGizmoSettings>,
) {
    if !ui_state.pointer_in_viewport || mouse.pressed(MouseButton::Right) {
        return;
    }

    if keys.just_pressed(KeyCode::KeyW) {
        settings.mode = TransformGizmoMode::Translate;
    }
    if keys.just_pressed(KeyCode::KeyE) {
        settings.mode = TransformGizmoMode::Rotate;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        settings.mode = TransformGizmoMode::Scale;
    }
    if keys.just_pressed(KeyCode::KeyX) {
        settings.space = match settings.space {
            TransformGizmoSpace::World => TransformGizmoSpace::Local,
            TransformGizmoSpace::Local => TransformGizmoSpace::World,
        };
    }
}

fn draw_mesh_intersections(pointers: Query<&PointerInteraction>, mut gizmos: Gizmos) {
    for (point, normal) in pointers
        .iter()
        .filter_map(|interaction| interaction.get_nearest_hit())
        .filter_map(|(_entity, hit)| hit.position.zip(hit.normal))
    {
        gizmos.sphere(point, 0.05, RED_500);
        gizmos.arrow(point, point + normal.normalize() * 0.5, PINK_100);
    }
}

/// Selects the entity under the pointer. The gizmo owns one entity at a time, so a
/// viewport click replaces the selection rather than extending it; the hierarchy panel
/// is where multi-selection still lives.
fn handle_pick_events(
    mut ui_state: ResMut<UiState>,
    mut click_events: MessageReader<PointerInput>,
    pointers: Query<&PointerInteraction>,
    gizmo: Res<TransformGizmoState>,
) {
    if !ui_state.pointer_in_viewport {
        return;
    }

    for event in click_events.read() {
        if !matches!(event.action, PointerAction::Press(PointerButton::Primary)) {
            continue;
        }
        // A press the gizmo is consuming must not fall through to the mesh behind the
        // handle. `hovered_axis` is cleared once a drag starts, so both are needed.
        if gizmo.active || gizmo.hovered_axis.is_some() {
            continue;
        }

        for interaction in &pointers {
            if let Some((entity, _)) = interaction.get_nearest_hit() {
                ui_state.selected_entities.select_replace(*entity);
            }
        }
    }
}

/// Points the transform gizmo at the selection.
///
/// The gizmo manipulates exactly one entity, so it is focused only while the selection
/// holds exactly one transformable entity, and cleared otherwise. Deriving focus from the
/// selection every frame, instead of bookkeeping it wherever something selects, is what
/// makes a hierarchy-panel click move the gizmo without a viewport click after it.
fn sync_gizmo_focus(
    ui_state: Res<UiState>,
    transformable: Query<(), With<Transform>>,
    focused: Query<Entity, With<TransformGizmoFocus>>,
    mut commands: Commands,
) {
    let target = match *ui_state.selected_entities.as_slice() {
        [entity] if transformable.contains(entity) => Some(entity),
        _ => None,
    };

    for entity in &focused {
        if Some(entity) != target {
            commands.entity(entity).remove::<TransformGizmoFocus>();
        }
    }

    if let Some(entity) = target
        && !focused.contains(entity)
    {
        commands.entity(entity).insert(TransformGizmoFocus);
    }
}

fn show_ui_system(world: &mut World) {
    let Ok(egui_context) = world
        .query_filtered::<&mut EguiContext, With<PrimaryEguiContext>>()
        .single(world)
    else {
        return;
    };
    let mut egui_context = egui_context.clone();
    let ctx = egui_context.get_mut();

    // egui 0.34 deprecated the panel `show(ctx, ..)` entry points, so an integration builds its
    // own root `Ui` instead. Same shape as bevy-inspector-egui's egui_dock example.
    let mut ui = egui::Ui::new(
        ctx.clone(),
        "viewport".into(),
        UiBuilder::new()
            .layer_id(LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );

    world.resource_scope::<UiState, _>(|world, mut ui_state| ui_state.ui(world, &mut ui));
}

// make camera only render to view not obstructed by UI
fn set_camera_viewport(
    ui_state: Res<UiState>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut cam: Single<&mut Camera, With<EditorCamera>>,
    zoom_factor: Single<&EguiZoomFactor, With<PrimaryEguiContext>>,
) {
    let scale_factor = window.scale_factor() * zoom_factor.zoom_factor;

    let viewport_pos = ui_state.viewport_rect.left_top().to_vec2() * scale_factor;
    let viewport_size = ui_state.viewport_rect.size() * scale_factor;

    let physical_position = UVec2::new(viewport_pos.x as u32, viewport_pos.y as u32);
    let physical_size = UVec2::new(viewport_size.x as u32, viewport_size.y as u32);

    let rect = physical_position + physical_size;

    let window_size = window.physical_size();
    if rect.x <= window_size.x && rect.y <= window_size.y {
        cam.viewport = Some(Viewport {
            physical_position,
            physical_size,
            depth: 0.0..1.0,
        });
    }
}

#[derive(Eq, PartialEq)]
enum InspectorSelection {
    Entities,
    Resource(TypeId, String),
    Asset(TypeId, String, UntypedAssetId),
}

#[derive(Resource)]
struct UiState {
    state: DockState<EguiWindow>,
    viewport_rect: egui::Rect,
    selected_entities: SelectedEntities,
    selection: InspectorSelection,
    pointer_in_viewport: bool,
}

impl UiState {
    pub fn new() -> Self {
        let mut state = DockState::new(vec![EguiWindow::GameView]);
        let tree = state.main_surface_mut();
        let [game, _inspector] =
            tree.split_right(NodeIndex::root(), 0.75, vec![EguiWindow::Inspector]);
        let [game, _hierarchy] = tree.split_left(game, 0.2, vec![EguiWindow::Hierarchy]);
        let [_game, _bottom] =
            tree.split_below(game, 0.8, vec![EguiWindow::Resources, EguiWindow::Assets]);

        Self {
            state,
            selected_entities: SelectedEntities::default(),
            selection: InspectorSelection::Entities,
            viewport_rect: egui::Rect::NOTHING,
            pointer_in_viewport: false,
        }
    }

    fn ui(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let mut tab_viewer = TabViewer {
            world,
            viewport_rect: &mut self.viewport_rect,
            selected_entities: &mut self.selected_entities,
            selection: &mut self.selection,
            pointer_in_viewport: &mut self.pointer_in_viewport,
        };
        DockArea::new(&mut self.state)
            .style(Style::from_egui(ui.style().as_ref()))
            .show_inside(ui, &mut tab_viewer);
    }
}

#[derive(Debug)]
enum EguiWindow {
    GameView,
    Hierarchy,
    Resources,
    Assets,
    Inspector,
}

struct TabViewer<'a> {
    world: &'a mut World,
    selected_entities: &'a mut SelectedEntities,
    selection: &'a mut InspectorSelection,
    viewport_rect: &'a mut egui::Rect,
    pointer_in_viewport: &'a mut bool,
}

impl egui_dock::TabViewer for TabViewer<'_> {
    type Tab = EguiWindow;

    fn ui(&mut self, ui: &mut egui_dock::egui::Ui, window: &mut Self::Tab) {
        let type_registry = self.world.resource::<AppTypeRegistry>().0.clone();
        let type_registry = type_registry.read();

        match window {
            EguiWindow::GameView => *self.viewport_rect = ui.clip_rect(),
            EguiWindow::Hierarchy => {
                let selected = hierarchy_ui(self.world, ui, self.selected_entities);
                if selected {
                    *self.selection = InspectorSelection::Entities;
                }
            }
            EguiWindow::Resources => select_resource(ui, &type_registry, self.selection),
            EguiWindow::Assets => select_asset(ui, &type_registry, self.world, self.selection),
            EguiWindow::Inspector => match *self.selection {
                InspectorSelection::Entities => match self.selected_entities.as_slice() {
                    &[entity] => ui_for_entity_with_children(self.world, entity, ui),
                    entities => ui_for_entities_shared_components(self.world, entities, ui),
                },
                InspectorSelection::Resource(type_id, ref name) => {
                    ui.label(name);
                    bevy_inspector::by_type_id::ui_for_resource(
                        self.world,
                        type_id,
                        ui,
                        name,
                        &type_registry,
                    )
                }
                InspectorSelection::Asset(type_id, ref name, handle) => {
                    ui.label(name);
                    bevy_inspector::by_type_id::ui_for_asset(
                        self.world,
                        type_id,
                        handle,
                        ui,
                        &type_registry,
                    );
                }
            },
        }

        *self.pointer_in_viewport = ui
            .ctx()
            .rect_contains_pointer(LayerId::background(), self.viewport_rect.shrink(16.));
    }

    fn title(&mut self, window: &mut Self::Tab) -> egui_dock::egui::WidgetText {
        format!("{window:?}").into()
    }

    fn clear_background(&self, window: &Self::Tab) -> bool {
        !matches!(window, EguiWindow::GameView)
    }
}

fn select_resource(
    ui: &mut egui::Ui,
    type_registry: &TypeRegistry,
    selection: &mut InspectorSelection,
) {
    let mut resources: Vec<_> = type_registry
        .iter()
        .filter(|registration| registration.data::<ReflectResource>().is_some())
        .map(|registration| {
            (
                registration.type_info().type_path_table().short_path(),
                registration.type_id(),
            )
        })
        .collect();
    resources.sort_by_key(|(name_a, _)| *name_a);

    for (resource_name, type_id) in resources {
        let selected = match *selection {
            InspectorSelection::Resource(selected, _) => selected == type_id,
            _ => false,
        };

        if ui.selectable_label(selected, resource_name).clicked() {
            *selection = InspectorSelection::Resource(type_id, resource_name.to_string());
        }
    }
}

fn select_asset(
    ui: &mut egui::Ui,
    type_registry: &TypeRegistry,
    world: &World,
    selection: &mut InspectorSelection,
) {
    let mut assets: Vec<_> = type_registry
        .iter()
        .filter_map(|registration| {
            let reflect_asset = registration.data::<ReflectAsset>()?;
            Some((
                registration.type_info().type_path_table().short_path(),
                registration.type_id(),
                reflect_asset,
            ))
        })
        .collect();
    assets.sort_by_key(|(name_a, ..)| *name_a);

    for (asset_name, asset_type_id, reflect_asset) in assets {
        let handles: Vec<_> = reflect_asset.ids(world).collect();

        ui.collapsing(format!("{asset_name} ({})", handles.len()), |ui| {
            for handle in handles {
                let selected = match *selection {
                    InspectorSelection::Asset(_, _, selected_id) => selected_id == handle,
                    _ => false,
                };

                if ui
                    .selectable_label(selected, format!("{handle:?}"))
                    .clicked()
                {
                    *selection =
                        InspectorSelection::Asset(asset_type_id, asset_name.to_string(), handle);
                }
            }
        });
    }
}

fn setup(mut commands: Commands, mut egui_global_settings: ResMut<EguiGlobalSettings>) {
    egui_global_settings.auto_create_primary_context = false;

    // egui camera
    commands.spawn((
        Camera2d,
        Name::new("Egui Camera"),
        PrimaryEguiContext,
        RenderLayers::none(),
        Camera {
            // Above the transform gizmo's overlay camera, which hardcodes `order: 1`.
            order: 2,
            clear_color: ClearColorConfig::None,
            ..default()
        },
    ));
}
