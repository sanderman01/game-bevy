//! The statistics panel: a feathers pane per host camera, top right of its viewport, showing the
//! mean of the last 0.5 s of [`PerfRecords`]. Behind the `overlay.stats` debug switch (`F3`).

use std::time::{Duration, Instant};

use bevy::{
    diagnostic::EntityCountDiagnosticsPlugin,
    ecs::entity::{EntityHashMap, EntityHashSet},
    feathers::{
        FeathersCorePlugin,
        constants::fonts,
        containers::{flex_spacer, pane, pane_body, pane_header},
        dark_theme::create_dark_theme,
        display::{label, label_dim},
        theme::{ThemeBackgroundColor, ThemeToken, ThemedText, UiTheme},
        tokens,
    },
    prelude::*,
    text::{FontSize, FontSourceTemplate},
    time::common_conditions::on_real_timer,
    window::PrimaryWindow,
};

use super::records::{PerfCounters, PerfRecords};
use crate::{
    camera::MainCameraFilter,
    debug_switch::{DebugSwitchAppExt, DebugSwitchSpec, KeyChord},
};

/// The window the panel's values are averaged over.
const MEAN_WINDOW: Duration = Duration::from_millis(500);
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Row {
    #[default]
    Fps,
    Frame,
    CpuMain,
    RenderWall,
    CpuView,
    GpuFrame,
    GpuView,
    Entities,
    /// Index into [`PerfCounters`].
    Counter(usize),
    Screen,
    View,
}

/// A row of the panel; hidden while its value has no source.
#[derive(Component, Clone, Copy, Debug, Default)]
struct StatsRow(Row);

/// The value text of a [`StatsRow`], its child.
#[derive(Component, Clone, Copy, Debug, Default)]
struct StatsValue;

pub(super) fn build(app: &mut App) {
    app.init_resource::<PerfOverlay>()
        .register_type::<PerfOverlay>()
        .register_type::<StatsPanelHost>()
        .add_field_debug_switch::<PerfOverlay>(
            DebugSwitchSpec::new(
                "overlay",
                "overlay.stats",
                "Draw the statistics panel: frame, CPU and GPU times per view.",
            )
            .key(KeyChord::new(KeyCode::F3)),
            "stats",
        )
        .add_systems(Update, reconcile_panels);
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
    app.add_observer(fill_panel).add_systems(
        Update,
        (
            add_stats_colors.run_if(resource_changed::<UiTheme>),
            refresh_panels
                .run_if(on_real_timer(REFRESH).or_else(any_match_filter::<Added<StatsRow>>)),
        ),
    );
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

/// Builds the feathers pane into a new panel root.
fn fill_panel(add: On<Add, StatsPanelOf>, mut commands: Commands, counters: Res<PerfCounters>) {
    let rows = |rows: &[(Row, &str)]| -> Vec<_> {
        rows.iter()
            .map(|(kind, name)| row(*kind, (*name).to_owned()))
            .collect()
    };
    let timing = rows(&[
        (Row::Fps, "FPS"),
        (Row::Frame, "Frame"),
        (Row::CpuMain, "CPU main"),
        (Row::RenderWall, "Render (wall)"),
        (Row::CpuView, "CPU view"),
        (Row::GpuFrame, "GPU frame"),
        (Row::GpuView, "GPU view"),
    ]);
    let mut counts = rows(&[(Row::Entities, "Entities")]);
    counts.extend(
        counters
            .0
            .iter()
            .enumerate()
            .filter(|(_, path)| **path != EntityCountDiagnosticsPlugin::ENTITY_COUNT)
            .map(|(index, path)| row(Row::Counter(index), path.to_string())),
    );
    let sizes = rows(&[(Row::Screen, "Screen"), (Row::View, "View")]);

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
                        click_through()
                        Children [
                            (group() Children [ {timing} ]),
                            (group() Children [ {counts} ]),
                            (group() Children [ {sizes} ]),
                        ]
                    ),
                ]
            ),
        ]
    });
}

fn group() -> impl Scene {
    bsn! {
        Node {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            row_gap: px(1),
        }
        click_through()
    }
}

fn row(kind: Row, name: String) -> impl Scene {
    bsn! {
        Node {
            display: Display::None,
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(12),
        }
        StatsRow({kind})
        click_through()
        Children [
            (label_dim(name) TextFont { font_size: TEXT_SIZE } click_through()),
            (flex_spacer() click_through()),
            (
                label("")
                TextFont { font: FontSourceTemplate::Handle(fonts::MONO), font_size: TEXT_SIZE }
                StatsValue
                click_through()
            ),
        ]
    }
}

/// bevy_ui treats a node without `Pickable` as blocking, so every panel node needs this to let
/// clicks reach the scene.
fn click_through() -> impl Scene {
    bsn! {
        Pickable { should_block_lower: false, is_hoverable: false }
    }
}

/// The panel values of one host, `None` where the source is missing.
#[derive(Default)]
struct Values {
    frame_ms: Option<f64>,
    cpu_main_ms: Option<f64>,
    render_wall_ms: Option<f64>,
    cpu_view_ms: Option<f64>,
    gpu_frame_ms: Option<f64>,
    gpu_view_ms: Option<f64>,
    entities: Option<f64>,
    counters: Vec<Option<f64>>,
    screen: Option<UVec2>,
    view: Option<UVec2>,
}

impl Values {
    fn of(records: &PerfRecords, host: Entity, now: Instant) -> Self {
        let recent = |at: Instant| now.saturating_duration_since(at) <= MEAN_WINDOW;
        let main: Vec<_> = records.main.iter().filter(|m| recent(m.at)).collect();
        let cpu: Vec<_> = records.cpu.iter().filter(|c| recent(c.at)).collect();
        let gpu: Vec<_> = records.gpu.iter().filter(|g| recent(g.at)).collect();
        let latest = records.main.back().map(|m| &m.record);
        Self {
            frame_ms: mean(main.iter().map(|m| m.record.frame_ms)),
            cpu_main_ms: mean(main.iter().map(|m| m.record.cpu_main_ms)),
            render_wall_ms: mean(cpu.iter().map(|c| c.record.render_wall_ms)),
            cpu_view_ms: mean(cpu.iter().filter_map(|c| view_ms(&c.record.views, host))),
            gpu_frame_ms: mean(gpu.iter().map(|g| g.record.total_ms)),
            gpu_view_ms: mean(gpu.iter().filter_map(|g| view_ms(&g.record.views, host))),
            entities: latest.and_then(|m| m.entity_count),
            counters: latest.map(|m| m.counters.clone()).unwrap_or_default(),
            screen: None,
            view: None,
        }
    }

    fn text(&self, row: Row) -> Option<String> {
        let ms = |value: Option<f64>| value.map(|ms| format!("{ms:.2} ms"));
        let size = |value: Option<UVec2>| value.map(|size| format!("{} × {}", size.x, size.y));
        match row {
            Row::Fps => self
                .frame_ms
                .filter(|ms| *ms > 0.0)
                .map(|ms| format!("{:.1}", 1000.0 / ms)),
            Row::Frame => ms(self.frame_ms),
            Row::CpuMain => ms(self.cpu_main_ms),
            Row::RenderWall => ms(self.render_wall_ms),
            Row::CpuView => ms(self.cpu_view_ms),
            Row::GpuFrame => ms(self.gpu_frame_ms),
            Row::GpuView => ms(self.gpu_view_ms),
            Row::Entities => self.entities.map(|count| format!("{count:.0}")),
            Row::Counter(index) => self
                .counters
                .get(index)
                .copied()
                .flatten()
                .map(|value| format!("{value:.2}")),
            Row::Screen => size(self.screen),
            Row::View => size(self.view),
        }
    }
}

/// The host's time in one frame, summed over its entries; `None` when it did not render.
fn view_ms(views: &[super::gpu_timer::ViewTime], host: Entity) -> Option<f64> {
    views
        .iter()
        .filter(|view| view.camera.id() == host)
        .map(|view| view.ms)
        .reduce(|a, b| a + b)
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0u32), |(sum, count), v| (sum + v, count + 1));
    (count > 0).then(|| sum / f64::from(count))
}

/// Re-texts the rows of every active host's panel, writing only what changed.
fn refresh_panels(
    records: Res<PerfRecords>,
    window: Query<&Window, With<PrimaryWindow>>,
    hosts: Query<&Camera>,
    panels: Query<&StatsPanelOf>,
    parents: Query<&ChildOf>,
    mut rows: Query<(Entity, &StatsRow, &mut Node, &Children)>,
    mut texts: Query<&mut Text, With<StatsValue>>,
) {
    let now = Instant::now();
    let screen = window.single().ok().map(Window::physical_size);
    let mut values = EntityHashMap::<Option<Values>>::default();
    for (entity, row, mut node, children) in &mut rows {
        let Some(host) = parents
            .iter_ancestors(entity)
            .find_map(|ancestor| panels.get(ancestor).ok())
            .map(|of| of.0)
        else {
            continue;
        };
        let host_values = values.entry(host).or_insert_with(|| {
            let camera = hosts.get(host).ok().filter(|camera| camera.is_active)?;
            Some(Values {
                screen,
                view: camera.physical_viewport_size(),
                ..Values::of(&records, host, now)
            })
        });
        let Some(host_values) = host_values else {
            continue;
        };
        let text = host_values.text(row.0);
        let display = if text.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
        let Some(text) = text else {
            continue;
        };
        for child in children {
            if let Ok(mut value) = texts.get_mut(*child)
                && value.0 != text
            {
                value.0.clone_from(&text);
            }
        }
    }
}
