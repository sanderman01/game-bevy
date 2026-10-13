//! The dock and its panels: hierarchy, inspector, console, resource and asset pickers.

use crate::console::ConsoleState;
use bevy::{
    asset::{ReflectAsset, UntypedAssetId},
    camera::{CameraOutputMode, visibility::RenderLayers},
    prelude::*,
    reflect::TypeRegistry,
    render::render_resource::BlendState,
};
use bevy_egui::{EguiGlobalSettings, EguiPrimaryContextPass, PrimaryEguiContext};
use bevy_inspector_egui::DefaultInspectorConfigPlugin;
use bevy_inspector_egui::bevy_egui::EguiContext;
use bevy_inspector_egui::bevy_inspector::hierarchy::{SelectedEntities, hierarchy_ui};
use bevy_inspector_egui::bevy_inspector::{
    self, ui_for_entities_shared_components, ui_for_entity_with_children,
};
use egui::{LayerId, UiBuilder};
use egui_dock::{DockArea, DockState, NodeIndex, Style};
use std::any::TypeId;

/// Owns the dock layout, the egui context, and the inspector panels.
pub(crate) struct PanelsPlugin;

impl Plugin for PanelsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(bevy_egui::EguiPlugin::default())
            .add_plugins(DefaultInspectorConfigPlugin)
            .insert_resource(UiState::new())
            .add_systems(Startup, spawn_egui_camera)
            .add_systems(EguiPrimaryContextPass, show_ui_system)
            .register_type::<Option<Handle<Image>>>()
            .register_type::<AlphaMode>();
    }
}

#[derive(Eq, PartialEq)]
enum InspectorSelection {
    Entities,
    Resource(TypeId, String),
    Asset(TypeId, String, UntypedAssetId),
}

/// Which of Scene View / Game View is shown, and so which camera is live.
///
/// Decided once per pass by [`resolve_viewport`], after the dock has drawn, rather than by
/// whichever viewport tab happened to draw last: with the two tabs in separate splits both draw
/// every pass. `viewport.rs` uses it to decide which camera is live, and `gizmo`/`selection` use
/// it to confirm the pointer is over Scene View specifically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ActiveViewport {
    #[default]
    Scene,
    Game,
}

#[derive(Resource)]
pub(crate) struct UiState {
    state: DockState<EguiWindow>,
    pub(crate) active_viewport: ActiveViewport,
    pub(crate) viewport_rect: egui::Rect,
    pub(crate) selected_entities: SelectedEntities,
    selection: InspectorSelection,
    pub(crate) pointer_in_viewport: bool,
    console: ConsoleState,
}

impl UiState {
    pub(crate) fn new() -> Self {
        let mut state = DockState::new(vec![EguiWindow::SceneView, EguiWindow::GameView]);
        let tree = state.main_surface_mut();
        let [game, _inspector] =
            tree.split_right(NodeIndex::root(), 0.75, vec![EguiWindow::Inspector]);
        let [game, _hierarchy] = tree.split_left(game, 0.2, vec![EguiWindow::Hierarchy]);
        // Console first, because `egui_dock`'s `Leaf::new` makes tab zero the active one.
        let [_game, _bottom] = tree.split_below(
            game,
            0.8,
            vec![
                EguiWindow::Console,
                EguiWindow::Resources,
                EguiWindow::Assets,
            ],
        );

        Self {
            state,
            active_viewport: ActiveViewport::default(),
            selected_entities: SelectedEntities::default(),
            selection: InspectorSelection::Entities,
            viewport_rect: egui::Rect::NOTHING,
            pointer_in_viewport: false,
            console: ConsoleState::default(),
        }
    }

    fn ui(&mut self, world: &mut World, ui: &mut egui::Ui) {
        let mut drawn = DrawnViewports::default();
        let mut tab_viewer = TabViewer {
            world,
            drawn: &mut drawn,
            selected_entities: &mut self.selected_entities,
            selection: &mut self.selection,
            console: &mut self.console,
        };
        // No collapse buttons: egui_dock 0.19 can collapse a leaf or minimize a window but has
        // no public way to expand or restore one, so `show_viewport` could not undo it.
        DockArea::new(&mut self.state)
            .style(Style::from_egui(ui.style().as_ref()))
            .show_leaf_collapse_buttons(false)
            .show_inside(ui, &mut tab_viewer);

        // After `show_inside`, which is where a click this pass moves the dock's focus.
        let focused = self.focused_viewport();
        self.active_viewport = resolve_viewport(self.active_viewport, &drawn, focused);
        if let Some(rect) = drawn.rect(self.active_viewport) {
            self.viewport_rect = rect;
        }
        self.pointer_in_viewport = ui
            .ctx()
            .rect_contains_pointer(LayerId::background(), self.viewport_rect.shrink(16.));
    }

    /// The viewport whose tab is the active one in the dock's focused leaf, if either is.
    fn focused_viewport(&self) -> Option<ActiveViewport> {
        let path = self.state.focused_leaf()?;
        let leaf = self.state.leaf(path).ok()?;
        match leaf.tabs().get(leaf.active.0)? {
            EguiWindow::SceneView => Some(ActiveViewport::Scene),
            EguiWindow::GameView => Some(ActiveViewport::Game),
            _ => None,
        }
    }

    /// Shows `viewport` as clicking its tab would: makes it the active tab of its leaf and
    /// focuses that leaf, so [`resolve_viewport`] keeps it on every later pass.
    ///
    /// Takes effect in `active_viewport` at once; the layout and cameras follow on the next pass.
    pub(crate) fn show_viewport(&mut self, viewport: ActiveViewport) -> Result<(), String> {
        let tab = match viewport {
            ActiveViewport::Scene => EguiWindow::SceneView,
            ActiveViewport::Game => EguiWindow::GameView,
        };
        let path = self
            .state
            .find_tab(&tab)
            .ok_or_else(|| format!("the {tab:?} tab was closed and the editor cannot reopen it"))?;
        // Unreachable with the collapse buttons hidden, but a selected tab in a collapsed leaf
        // would report success and never draw.
        let tree = &self.state[path.surface];
        let mut node = Some(path.node);
        while let Some(index) = node {
            if tree[index].is_collapsed() {
                return Err(format!(
                    "the {tab:?} tab is in a collapsed dock panel; expand it first"
                ));
            }
            node = index.parent();
        }
        self.state
            .set_active_tab(path)
            .map_err(|err| format!("cannot show the {tab:?} tab: {err:?}"))?;
        self.state.set_focused_node_and_surface(path.node_path());
        self.active_viewport = viewport;
        Ok(())
    }
}

/// Where each viewport tab drew this pass, or `None` for a tab that did not draw.
#[derive(Default)]
struct DrawnViewports {
    scene: Option<egui::Rect>,
    game: Option<egui::Rect>,
}

impl DrawnViewports {
    fn rect(&self, viewport: ActiveViewport) -> Option<egui::Rect> {
        match viewport {
            ActiveViewport::Scene => self.scene,
            ActiveViewport::Game => self.game,
        }
    }
}

/// Which viewport is shown after a pass.
///
/// The only one drawn wins, which is every pass while the two share a leaf. When both drew, they
/// are in separate splits, and the one in the focused leaf wins: egui_dock moves focus on a click
/// on a tab or inside a leaf, and a remote switch focuses the leaf too. Otherwise nothing changes.
fn resolve_viewport(
    previous: ActiveViewport,
    drawn: &DrawnViewports,
    focused: Option<ActiveViewport>,
) -> ActiveViewport {
    match (drawn.scene.is_some(), drawn.game.is_some()) {
        (true, false) => ActiveViewport::Scene,
        (false, true) => ActiveViewport::Game,
        (true, true) => focused.unwrap_or(previous),
        (false, false) => previous,
    }
}

#[derive(Debug, PartialEq)]
enum EguiWindow {
    SceneView,
    GameView,
    Console,
    Hierarchy,
    Resources,
    Assets,
    Inspector,
}

struct TabViewer<'a> {
    world: &'a mut World,
    selected_entities: &'a mut SelectedEntities,
    selection: &'a mut InspectorSelection,
    drawn: &'a mut DrawnViewports,
    console: &'a mut ConsoleState,
}

impl egui_dock::TabViewer for TabViewer<'_> {
    type Tab = EguiWindow;

    fn ui(&mut self, ui: &mut egui_dock::egui::Ui, window: &mut Self::Tab) {
        let type_registry = self.world.resource::<AppTypeRegistry>().0.clone();
        let type_registry = type_registry.read();

        match window {
            EguiWindow::SceneView => self.drawn.scene = Some(ui.clip_rect()),
            EguiWindow::GameView => {
                self.drawn.game = Some(ui.clip_rect());
                crate::game_view::ui(ui, self.world);
            }
            EguiWindow::Hierarchy => {
                let selected = hierarchy_ui(self.world, ui, self.selected_entities);
                if selected {
                    *self.selection = InspectorSelection::Entities;
                }
            }
            EguiWindow::Console => crate::console::ui(ui, self.console, self.world),
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
    }

    fn title(&mut self, window: &mut Self::Tab) -> egui_dock::egui::WidgetText {
        format!("{window:?}").into()
    }

    /// Scene View is the one tab egui must not paint over: its camera renders straight into the
    /// window behind the dock. Game View arrives as an egui texture and needs a background under
    /// it, for the frames where there is no stage camera to draw.
    fn clear_background(&self, window: &Self::Tab) -> bool {
        !matches!(window, EguiWindow::SceneView)
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

    world.resource_scope::<UiState, _>(|world, mut ui_state| {
        // Claims a horizontal strip at the top of `ui` and advances `ui`'s own cursor past it,
        // the same way `console.rs`'s `egui::Panel::bottom(...)` claims the detail pane's slice
        // of its tab -- so the dock below only ever sees what's left.
        egui::Panel::top("menu_bar").show_inside(&mut ui, |ui| {
            crate::menu::ui(ui, world, &mut ui_state.selected_entities);
        });
        ui_state.ui(world, &mut ui);
    });
}

fn spawn_egui_camera(mut commands: Commands, mut egui_global_settings: ResMut<EguiGlobalSettings>) {
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
            // Cleared to transparent and blended over the window, so the UI composes over the scene
            // cameras' output whether or not they share this camera's texture (an HDR one does not).
            clear_color: ClearColorConfig::Custom(Color::NONE),
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            ..default()
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui_state(state: DockState<EguiWindow>) -> UiState {
        UiState {
            state,
            ..UiState::new()
        }
    }

    /// Two viewports in one leaf, as the default layout has them.
    fn shared_leaf() -> UiState {
        ui_state(DockState::new(vec![
            EguiWindow::SceneView,
            EguiWindow::GameView,
        ]))
    }

    /// Scene View and Game View side by side, so both draw every pass.
    fn split_leaves() -> UiState {
        let mut state = DockState::new(vec![EguiWindow::SceneView]);
        state
            .main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec![EguiWindow::GameView]);
        ui_state(state)
    }

    /// One headless egui pass over the dock, the same call `show_ui_system` makes.
    fn pass(ui_state: &mut UiState, world: &mut World, ctx: &egui::Context) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..default()
        };
        let _ = ctx.run_ui(input, |ui| ui_state.ui(world, ui));
    }

    fn world() -> World {
        let mut world = World::new();
        world.init_resource::<AppTypeRegistry>();
        world
    }

    #[test]
    fn a_switch_in_a_shared_leaf_survives_later_passes() {
        let (mut world, ctx, mut state) = (world(), egui::Context::default(), shared_leaf());
        pass(&mut state, &mut world, &ctx);
        assert_eq!(state.active_viewport, ActiveViewport::Scene);

        state.show_viewport(ActiveViewport::Game).unwrap();
        assert_eq!(state.active_viewport, ActiveViewport::Game);
        pass(&mut state, &mut world, &ctx);
        pass(&mut state, &mut world, &ctx);
        assert_eq!(state.active_viewport, ActiveViewport::Game);
        assert!(state.viewport_rect.is_positive());
    }

    #[test]
    fn a_switch_between_split_leaves_survives_later_passes() {
        let (mut world, ctx, mut state) = (world(), egui::Context::default(), split_leaves());
        pass(&mut state, &mut world, &ctx);
        assert_eq!(state.active_viewport, ActiveViewport::Scene);
        let scene_rect = state.viewport_rect;

        state.show_viewport(ActiveViewport::Game).unwrap();
        pass(&mut state, &mut world, &ctx);
        pass(&mut state, &mut world, &ctx);
        assert_eq!(state.active_viewport, ActiveViewport::Game);
        assert_ne!(state.viewport_rect, scene_rect);

        state.show_viewport(ActiveViewport::Scene).unwrap();
        pass(&mut state, &mut world, &ctx);
        pass(&mut state, &mut world, &ctx);
        assert_eq!(state.active_viewport, ActiveViewport::Scene);
        assert_eq!(state.viewport_rect, scene_rect);
    }

    #[test]
    fn showing_a_tab_in_a_collapsed_leaf_fails_and_changes_nothing() {
        let mut state = split_leaves();
        let path = state.state.find_tab(&EguiWindow::GameView).unwrap();
        state.state[path.surface][path.node].set_collapsed(true);
        assert!(state.show_viewport(ActiveViewport::Game).is_err());
        assert_eq!(state.active_viewport, ActiveViewport::Scene);
        assert_eq!(state.state.focused_leaf(), None);
    }

    #[test]
    fn showing_a_closed_tab_fails_and_changes_nothing() {
        let mut state = ui_state(DockState::new(vec![EguiWindow::SceneView]));
        assert!(state.show_viewport(ActiveViewport::Game).is_err());
        assert_eq!(state.active_viewport, ActiveViewport::Scene);
    }

    #[test]
    fn the_only_viewport_drawn_wins_and_the_focused_one_breaks_a_tie() {
        use ActiveViewport::{Game, Scene};
        let rect = Some(egui::Rect::EVERYTHING);
        let scene = DrawnViewports {
            scene: rect,
            game: None,
        };
        let game = DrawnViewports {
            scene: None,
            game: rect,
        };
        let both = DrawnViewports {
            scene: rect,
            game: rect,
        };
        let neither = DrawnViewports::default();

        assert_eq!(resolve_viewport(Game, &scene, Some(Game)), Scene);
        assert_eq!(resolve_viewport(Scene, &game, None), Game);
        assert_eq!(resolve_viewport(Scene, &both, Some(Game)), Game);
        assert_eq!(resolve_viewport(Game, &both, None), Game);
        assert_eq!(resolve_viewport(Game, &neither, Some(Scene)), Game);
    }
}
