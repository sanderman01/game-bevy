//! The statistics panel: a feathers pane per host camera, top right of its viewport, showing the
//! [`StatsSection`]s and [`StatsRow`]s under the [`StatsContent`] root. The built-in rows cover
//! the last [`PANEL_FRAMES`] frames of `PerfRecords`. Behind the `overlay.stats` debug switch
//! (`F3`). `panel/content.rs` says how other code adds sections and rows.

pub(crate) mod builtin;
pub(crate) mod content;

use std::time::Duration;

use bevy::{
    ecs::entity::EntityHashSet,
    feathers::{
        FeathersCorePlugin,
        constants::fonts,
        containers::{pane, pane_body, pane_header},
        dark_theme::create_dark_theme,
        display::{label, label_dim},
        theme::{ThemeBackgroundColor, ThemeToken, ThemedText, UiTheme},
        tokens,
    },
    prelude::*,
    scene::{SpawnSceneError, WorldSceneExt},
    text::{FontSize, FontSourceTemplate},
    time::common_conditions::on_real_timer,
};

pub use builtin::{StatsFpsSection, StatsSceneSection, StatsTimingSection};
pub use content::{
    StatsCells, StatsContent, StatsRow, StatsRowSource, StatsRowSystem, StatsSection,
};

use content::{Layout, StatsPanelValues, StatsWarnings};

use crate::{
    camera::MainCameraFilter,
    debug_switch::{DebugSwitchAppExt, DebugSwitchSpec, KeyChord},
};

/// How many of the newest frames the built-in values cover.
pub(super) const PANEL_FRAMES: usize = 120;
/// How often the values are re-texted.
const REFRESH: Duration = Duration::from_millis(250);
/// Above game UI.
const PANEL_Z: i32 = 1 << 20;
const TEXT_SIZE: FontSize = FontSize::Px(12.0);

/// Panel colours of our own, so the pane can be translucent over the scene.
const STATS_BG: ThemeToken = ThemeToken::new_static("ename.stats.bg");
const STATS_HEADER_BG: ThemeToken = ThemeToken::new_static("ename.stats.header.bg");

/// The statistics overlay's debug switch.
#[derive(Resource, Debug, Clone, Copy, Default, Reflect)]
#[reflect(Resource, Default)]
pub struct PerfOverlay {
    /// Draw the statistics panel on every host camera.
    pub stats: bool,
}

/// A camera that gets a statistics panel while `overlay.stats` is on. The `MainCameraFilter`
/// camera gets one without it.
#[derive(Component, Reflect, Clone, Copy, Debug, Default)]
#[reflect(Component, Default)]
pub struct StatsPanelHost;

/// On a panel root: the camera it reports on. Not reflected, so stages never save it.
#[derive(Component, Debug)]
#[relationship(relationship_target = StatsPanels)]
pub struct StatsPanelOf(pub Entity);

/// On a host camera: its panels, despawned with it.
#[derive(Component, Debug)]
#[relationship_target(relationship = StatsPanelOf, linked_spawn)]
pub struct StatsPanels(Vec<Entity>);

/// The pane body the sections are built into.
#[derive(Component, Clone, Copy, Debug, Default)]
struct StatsPanelBody;

/// On a panel root: the layout its body was built from, and the nodes built for it.
#[derive(Component, Debug)]
struct BuiltBody {
    layout: Layout,
    sections: BuiltSections,
}

/// Per section of a layout: its grid node, and per row the label and value cells.
type BuiltSections = Vec<(Entity, Vec<Vec<Entity>>)>;

pub(super) fn build(app: &mut App) {
    content::build(app);
    builtin::build(app);
    app.init_resource::<PerfOverlay>()
        .register_type::<PerfOverlay>()
        .register_type::<StatsPanelHost>()
        .add_field_debug_switch::<PerfOverlay>(
            DebugSwitchSpec::new(
                "overlay",
                "overlay.stats",
                "Draw the statistics panel: FPS, frame, CPU and GPU time, entities, view size.",
            )
            .key(KeyChord::new(KeyCode::F3)),
            "stats",
        )
        .add_systems(
            Update,
            (
                reconcile_panels,
                refresh_panels.run_if((|overlay: Res<PerfOverlay>| overlay.stats).and_then(
                    on_real_timer(REFRESH).or_else(any_match_filter::<Added<StatsPanelOf>>),
                )),
            )
                .chain(),
        );
}

/// Feathers draws the panel; without `FeathersCorePlugin` the roots stay empty.
pub(super) fn finish(app: &mut App) {
    if !app.is_plugin_added::<FeathersCorePlugin>() {
        return;
    }
    if let Some(mut theme) = app.world_mut().get_resource_mut::<UiTheme>()
        && theme.0.color.is_empty()
    {
        *theme = UiTheme(create_dark_theme());
    }
    app.add_observer(fill_panel)
        .add_systems(Update, add_stats_colors.run_if(resource_changed::<UiTheme>));
}

/// Adds the panel's colours to a theme that lacks them, derived from its pane colours.
fn add_stats_colors(mut theme: ResMut<UiTheme>) {
    let colors = &theme.0.color;
    let missing = |token: &ThemeToken| !colors.contains_key(token);
    if !missing(&STATS_BG) && !missing(&STATS_HEADER_BG) {
        return;
    }
    let base = |token, fallback: Color| colors.get(&token).copied().unwrap_or(fallback);
    let body = base(tokens::PANE_BODY_BG, Color::srgb(0.13, 0.13, 0.14)).with_alpha(0.86);
    let header = base(tokens::PANE_HEADER_BG, Color::srgb(0.09, 0.09, 0.1)).with_alpha(0.92);
    theme.0.color.entry(STATS_BG).or_insert(body);
    theme.0.color.entry(STATS_HEADER_BG).or_insert(header);
}

/// Gives every qualifying host a panel and despawns the panels of hosts that no longer qualify.
fn reconcile_panels(
    mut commands: Commands,
    overlay: Res<PerfOverlay>,
    hosts: Query<Entity, (With<Camera>, With<StatsPanelHost>)>,
    main_camera: Query<Entity, MainCameraFilter>,
    panels: Query<(Entity, &StatsPanelOf)>,
) {
    if !overlay.stats && panels.is_empty() {
        return;
    }
    let wanted = overlay
        .stats
        .then(|| hosts.iter().chain(main_camera.single().ok()))
        .into_iter()
        .flatten();
    let panels = panels.iter().map(|(panel, of)| (panel, of.0));
    let (despawn, spawn) = reconcile(wanted, panels);
    for panel in despawn {
        commands.entity(panel).despawn();
    }
    for host in spawn {
        commands.spawn((
            Name::new("Statistics panel"),
            StatsPanelOf(host),
            UiTargetCamera(host),
        ));
    }
}

/// The `(panel, host)` pairs to despawn and the hosts to give a panel, so each wanted host has
/// exactly one. A host can be wanted twice: the main camera may also be a `StatsPanelHost`.
pub(crate) fn reconcile(
    wanted: impl IntoIterator<Item = Entity>,
    panels: impl IntoIterator<Item = (Entity, Entity)>,
) -> (Vec<Entity>, Vec<Entity>) {
    let mut missing: EntityHashSet = wanted.into_iter().collect();
    let mut despawn = Vec::new();
    for (panel, host) in panels {
        // The first panel of a wanted host stays; later ones and unwanted ones go.
        if !missing.remove(&host) {
            despawn.push(panel);
        }
    }
    let mut spawn: Vec<Entity> = missing.into_iter().collect();
    spawn.sort();
    (despawn, spawn)
}

/// Builds the feathers pane into a new panel root; the refresh fills its body.
fn fill_panel(add: On<Add, StatsPanelOf>, mut commands: Commands) {
    commands.entity(add.entity).apply_scene(bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: px(8),
            right: px(8),
            min_width: px(190),
        }
        GlobalZIndex(PANEL_Z)
        click_through()
        Children [
            (
                pane() click_through() Children [
                    (
                        pane_header()
                        Node { min_height: px(24), padding: px(4) }
                        ThemeBackgroundColor(STATS_HEADER_BG)
                        click_through()
                        Children [ (Text("Statistics") ThemedText click_through()) ]
                    ),
                    (
                        pane_body()
                        Node { row_gap: px(6), padding: px(6) }
                        ThemeBackgroundColor(STATS_BG)
                        StatsPanelBody
                        click_through()
                    ),
                ]
            ),
        ]
    });
}

/// bevy_ui treats a node without `Pickable` as blocking, so every panel node needs this to let
/// clicks reach the scene.
fn click_through() -> impl Scene {
    bsn! {
        Pickable { should_block_lower: false, is_hoverable: false }
    }
}

/// Every refresh: the content's layout, each panel's values, and the panels' UI, rebuilt where
/// the layout changed. Exclusive, since row systems run with `run_system_with`.
pub(crate) fn refresh_panels(world: &mut World) {
    builtin::update_summary(world);
    let mut warnings = world.remove_resource::<StatsWarnings>().unwrap_or_default();
    warnings.prune(world);
    let layout = content::layout(world, &mut warnings);
    let mut panels = world.query::<(Entity, &StatsPanelOf)>();
    let panels: Vec<(Entity, Entity)> = panels
        .iter(world)
        .map(|(panel, of)| (panel, of.0))
        .collect();
    for (panel, host) in panels {
        // Hidden views stay current too, so switching views never shows an empty panel.
        if world.get::<Camera>(host).is_none() {
            continue;
        }
        let values = content::values(world, &layout, host, &mut warnings);
        // A row's system may have despawned the panel.
        if world.get_entity(panel).is_err() {
            continue;
        }
        sync_body(world, panel, &layout, &values);
        world.entity_mut(panel).insert(values);
    }
    world.insert_resource(warnings);
}

/// The body of `panel`, once feathers has built the pane.
fn panel_body(world: &mut World, panel: Entity) -> Option<Entity> {
    let mut bodies = world.query_filtered::<Entity, With<StatsPanelBody>>();
    bodies.iter(world).find(|&body| {
        let mut entity = body;
        while let Some(parent) = world.get::<ChildOf>(entity) {
            entity = parent.parent();
            if entity == panel {
                return true;
            }
        }
        false
    })
}

/// Rebuilds the body when its layout is stale, then writes values and visibility.
fn sync_body(world: &mut World, panel: Entity, layout: &Layout, values: &StatsPanelValues) {
    let Some(body) = panel_body(world, panel) else {
        return;
    };
    if world
        .get::<BuiltBody>(panel)
        .is_none_or(|built| built.layout != *layout)
    {
        world.entity_mut(panel).remove::<BuiltBody>();
        match build_body(world, body, layout) {
            Ok(sections) => {
                world.entity_mut(panel).insert(BuiltBody {
                    layout: layout.clone(),
                    sections,
                });
            }
            Err(error) => {
                warn!("Statistics panel {panel}: could not build its rows: {error}");
                return;
            }
        }
    }
    let Some(built) = world.get::<BuiltBody>(panel) else {
        return;
    };
    let sections = built.sections.clone();
    for (section, (node, rows)) in layout.0.iter().zip(sections) {
        let shown = values.shows(section);
        set_display(
            world,
            node,
            if shown { Display::Grid } else { Display::None },
        );
        for (row, cells) in section.rows.iter().zip(rows) {
            let row_values = values.row(row.entity);
            let display = if row_values.is_some() {
                Display::Flex
            } else {
                Display::None
            };
            for (column, cell) in cells.into_iter().enumerate() {
                set_display(world, cell, display);
                if let Some(text) =
                    row_values.and_then(|v| column.checked_sub(1).and_then(|c| v.get(c)))
                    && let Some(mut value) = world.get_mut::<Text>(cell)
                    && value.0 != *text
                {
                    value.0.clone_from(text);
                }
            }
        }
    }
}

fn set_display(world: &mut World, node: Entity, display: Display) {
    if let Some(mut node) = world.get_mut::<Node>(node)
        && node.display != display
    {
        node.display = display;
    }
}

/// Despawns the body's children and spawns one grid per section, hidden until valued.
fn build_body(
    world: &mut World,
    body: Entity,
    layout: &Layout,
) -> Result<BuiltSections, SpawnSceneError> {
    world.entity_mut(body).despawn_related::<Children>();
    let mut sections = Vec::new();
    for section in &layout.0 {
        let columns = u16::try_from(section.columns).unwrap_or(u16::MAX);
        let grid = world
            .spawn((
                Node {
                    display: Display::None,
                    grid_template_columns: vec![
                        RepeatedGridTrack::flex(1, 1.0),
                        RepeatedGridTrack::auto(columns),
                    ],
                    column_gap: px(12),
                    row_gap: px(1),
                    ..default()
                },
                Pickable::IGNORE,
                ChildOf(body),
            ))
            .id();
        if let Some(title) = &section.title {
            let title = spawn_cell(world, grid, small(title.to_string()), JustifySelf::Start)?;
            if let Some(mut node) = world.get_mut::<Node>(title) {
                node.grid_column = GridPlacement::span(columns.saturating_add(1));
            }
        }
        if let Some(header) = &section.header {
            // Padded, so the rows start on a line of their own.
            let mut header = header.iter().map(|text| text.to_string());
            let first = header.next().unwrap_or_default();
            spawn_cell(world, grid, small_dim(first), JustifySelf::Start)?;
            for _ in 0..section.columns {
                let text = header.next().unwrap_or_default();
                spawn_cell(world, grid, mono_dim(text), JustifySelf::End)?;
            }
        }
        let mut rows = Vec::new();
        for row in &section.rows {
            let mut cells = vec![spawn_cell(
                world,
                grid,
                small_dim(row.label.to_string()),
                JustifySelf::Start,
            )?];
            for _ in 0..section.columns {
                cells.push(spawn_cell(world, grid, mono(), JustifySelf::End)?);
            }
            for &cell in &cells {
                set_display(world, cell, Display::None);
            }
            rows.push(cells);
        }
        sections.push((grid, rows));
    }
    Ok(sections)
}

fn spawn_cell(
    world: &mut World,
    grid: Entity,
    scene: impl Scene,
    justify: JustifySelf,
) -> Result<Entity, SpawnSceneError> {
    let mut cell = world.spawn_scene(scene)?;
    cell.insert((Pickable::IGNORE, ChildOf(grid)));
    let id = cell.id();
    if let Some(mut node) = world.get_mut::<Node>(id) {
        node.justify_self = justify;
    }
    Ok(id)
}

fn small(text: String) -> impl Scene {
    bsn! { label(text) TextFont { font_size: TEXT_SIZE } }
}

fn small_dim(text: String) -> impl Scene {
    bsn! { label_dim(text) TextFont { font_size: TEXT_SIZE } }
}

fn mono_dim(text: String) -> impl Scene {
    bsn! {
        label_dim(text)
        TextFont { font: FontSourceTemplate::Handle(fonts::MONO), font_size: TEXT_SIZE }
    }
}

fn mono() -> impl Scene {
    bsn! {
        label("")
        TextFont { font: FontSourceTemplate::Handle(fonts::MONO), font_size: TEXT_SIZE }
    }
}
