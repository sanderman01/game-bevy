//! What the statistics panel shows: sections and rows as entities under one [`StatsContent`]
//! root. Every panel is a view of them, so content outlives panels and other code adds to it at
//! runtime by spawning, and removes by despawning.
//!
//! ```ignore
//! fn add_stats(mut commands: Commands, scene: Single<Entity, With<StatsSceneSection>>) {
//!     // A section of its own, below the built-ins (orders 0, 10, 20).
//!     commands.spawn((
//!         StatsSection::titled("Streaming", 100).with_header(["", "tiles"]),
//!         children![StatsRow::system("Loaded", 0, loaded_tiles)],
//!     ));
//!     // One row in a built-in section.
//!     commands.spawn((StatsRow::diagnostic("Bodies", 20, BODY_COUNT), ChildOf(*scene)));
//! }
//!
//! fn loaded_tiles(_host: In<Entity>, tiles: Query<(), With<Tile>>) -> Option<StatsCells> {
//!     Some(StatsCells::one(tiles.iter().count().to_string()))
//! }
//! ```
//!
//! The built-ins are spawned in `PreStartup`, so add content from `Startup` on.

use std::borrow::Cow;

use bevy::{
    diagnostic::{DiagnosticPath, DiagnosticsStore},
    ecs::{
        entity::EntityHashMap,
        system::{BoxedSystem, RegisteredSystemError, SystemHandle, SystemId},
    },
    platform::collections::HashSet,
    prelude::*,
};

/// The root of the panel content; its children are the [`StatsSection`]s. One exists from
/// `PreStartup` on.
#[derive(Component, Debug, Default)]
#[require(Name = Name::new("Statistics content"))]
pub struct StatsContent;

/// A group of rows in the statistics panel, drawn as one grid. Its rows are its children; it is a
/// child of the [`StatsContent`] root, and a section spawned without a parent is put there.
///
/// `title` and `header` are optional parts of one presentation value, so they are fields
/// rather than marker components.
#[derive(Component, Clone, Debug, PartialEq)]
#[require(Name)]
pub struct StatsSection {
    /// Shown above the rows when set.
    pub title: Option<Cow<'static, str>>,
    /// A dim header row: the first entry heads the label column, the rest head the value
    /// columns. Without one the section has a single value column.
    pub header: Option<Vec<Cow<'static, str>>>,
    /// Sections are shown by ascending order; ties keep their order among the root's children.
    pub order: i32,
}

impl StatsSection {
    pub fn new(order: i32) -> Self {
        Self {
            title: None,
            header: None,
            order,
        }
    }

    pub fn titled(title: impl Into<Cow<'static, str>>, order: i32) -> Self {
        Self {
            title: Some(title.into()),
            ..Self::new(order)
        }
    }

    pub fn with_header<S: Into<Cow<'static, str>>>(
        mut self,
        header: impl IntoIterator<Item = S>,
    ) -> Self {
        self.header = Some(header.into_iter().map(Into::into).collect());
        self
    }

    /// The number of value cells each row shows: one per header entry after the first, at
    /// least one.
    pub fn value_columns(&self) -> usize {
        self.header
            .as_ref()
            .map_or(1, |header| header.len().saturating_sub(1).max(1))
    }
}

/// A row of the statistics panel; a child of a [`StatsSection`]. Spawn to add, despawn to remove.
#[derive(Component, Debug)]
#[require(Name)]
pub struct StatsRow {
    pub label: Cow<'static, str>,
    /// Rows are shown by ascending order; ties keep their order among the section's children.
    pub order: i32,
    pub source: StatsRowSource,
}

impl StatsRow {
    /// A row whose cells come from `system`, run once per panel with the panel's host camera.
    /// The row owns the system: despawning the row unregisters it. A system's `Local` state is
    /// shared by all hosts.
    pub fn system<M>(
        label: impl Into<Cow<'static, str>>,
        order: i32,
        system: impl IntoSystem<In<Entity>, Option<StatsCells>, M>,
    ) -> Self {
        Self {
            label: label.into(),
            order,
            source: StatsRowSource::System(StatsRowSystem(RowSystem::Pending(Some(Box::new(
                IntoSystem::into_system(system),
            ))))),
        }
    }

    /// A row showing a Bevy diagnostic's latest value.
    pub fn diagnostic(
        label: impl Into<Cow<'static, str>>,
        order: i32,
        path: DiagnosticPath,
    ) -> Self {
        Self {
            label: label.into(),
            order,
            source: StatsRowSource::Diagnostic(path),
        }
    }
}

/// Where a [`StatsRow`]'s cells come from.
#[derive(Debug)]
pub enum StatsRowSource {
    /// The diagnostic's latest value with its suffix, in one cell; whole numbers without
    /// decimals, others with two. Hidden while the diagnostic is missing, disabled or unmeasured.
    Diagnostic(DiagnosticPath),
    /// A system given the host camera, returning one cell per value column; `None` hides the
    /// row. Build it with [`StatsRow::system`].
    System(StatsRowSystem),
}

/// A row's system, registered by the panel the first time it is shown.
pub struct StatsRowSystem(RowSystem);

enum RowSystem {
    Pending(Option<BoxedSystem<In<Entity>, Option<StatsCells>>>),
    /// The strong handle: dropping it with the row despawns the system entity.
    Registered(SystemHandle<In<Entity>, Option<StatsCells>>),
}

impl std::fmt::Debug for StatsRowSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            RowSystem::Pending(_) => f.write_str("StatsRowSystem(pending)"),
            RowSystem::Registered(handle) => write!(f, "StatsRowSystem({:?})", handle.entity()),
        }
    }
}

/// The cells of one row, one per value column.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatsCells(pub Vec<String>);

impl StatsCells {
    pub fn one(text: impl Into<String>) -> Self {
        Self(vec![text.into()])
    }
}

impl<S: Into<String>> FromIterator<S> for StatsCells {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        Self(iter.into_iter().map(Into::into).collect())
    }
}

impl<S: Into<String>, const N: usize> From<[S; N]> for StatsCells {
    fn from(cells: [S; N]) -> Self {
        cells.into_iter().collect()
    }
}

/// The sorted content as the panel draws it. A panel whose body was built from a different
/// layout is rebuilt, which catches spawns, despawns, field edits, reparenting and child swaps.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Layout(pub Vec<SectionLayout>);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SectionLayout {
    pub entity: Entity,
    pub order: i32,
    pub title: Option<Cow<'static, str>>,
    pub header: Option<Vec<Cow<'static, str>>>,
    pub columns: usize,
    pub rows: Vec<RowLayout>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowLayout {
    pub entity: Entity,
    pub order: i32,
    pub label: Cow<'static, str>,
}

/// The cells of the rows a panel shows, by row entity, fitted to their section's columns.
#[derive(Component, Debug, Default)]
pub(crate) struct StatsPanelValues(pub EntityHashMap<Vec<String>>);

impl StatsPanelValues {
    pub(crate) fn row(&self, row: Entity) -> Option<&[String]> {
        self.0.get(&row).map(Vec::as_slice)
    }

    /// A section whose rows are all hidden is hidden too, title and header included.
    pub(crate) fn shows(&self, section: &SectionLayout) -> bool {
        section
            .rows
            .iter()
            .any(|row| self.0.contains_key(&row.entity))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum StatsWarning {
    /// A row not under a section of the root, or a section not under the root.
    Unplaced,
    /// The row's system could not run.
    Failed,
    /// The row's cells did not match its section's columns.
    Cells,
}

/// Warnings already logged, once per entity and kind.
#[derive(Resource, Debug, Default)]
pub(crate) struct StatsWarnings(pub HashSet<(Entity, StatsWarning)>);

impl StatsWarnings {
    /// True the first time, so the caller logs.
    fn first(&mut self, entity: Entity, warning: StatsWarning) -> bool {
        self.0.insert((entity, warning))
    }

    pub(crate) fn prune(&mut self, world: &World) {
        self.0
            .retain(|(entity, _)| world.get_entity(*entity).is_ok());
    }
}

pub(super) fn build(app: &mut App) {
    app.init_resource::<StatsWarnings>()
        .add_observer(place_section)
        .add_observer(name_section)
        .add_observer(name_row);
}

/// The content root, spawned if there is none yet.
pub(super) fn content_root(world: &mut World) -> Entity {
    let mut roots = world.query_filtered::<Entity, With<StatsContent>>();
    if let Some(root) = roots.iter(world).next() {
        return root;
    }
    world.spawn(StatsContent).id()
}

/// Puts a section spawned without a parent under the content root.
fn place_section(
    add: On<Add, StatsSection>,
    parented: Query<(), With<ChildOf>>,
    mut commands: Commands,
) {
    if parented.contains(add.entity) {
        return;
    }
    let section = add.entity;
    // Deferred, so a `ChildOf` inserted right after the spawn wins.
    commands.queue(move |world: &mut World| {
        if world.get_entity(section).is_err() || world.get::<ChildOf>(section).is_some() {
            return;
        }
        let root = content_root(world);
        world.entity_mut(section).insert(ChildOf(root));
    });
}

fn name_section(add: On<Add, StatsSection>, mut sections: Query<(&StatsSection, &mut Name)>) {
    if let Ok((section, mut name)) = sections.get_mut(add.entity)
        && name.as_str().is_empty()
    {
        name.set(section.title.clone().unwrap_or("Statistics section".into()));
    }
}

fn name_row(add: On<Add, StatsRow>, mut rows: Query<(&StatsRow, &mut Name)>) {
    if let Ok((row, mut name)) = rows.get_mut(add.entity)
        && name.as_str().is_empty()
    {
        name.set(row.label.clone());
    }
}

/// The sections under the content root and their rows, each sorted by order. Rows and
/// sections outside that tree warn once and are left out.
pub(crate) fn layout(world: &mut World, warnings: &mut StatsWarnings) -> Layout {
    let mut roots = world.query_filtered::<&Children, With<StatsContent>>();
    let mut sections = world.query::<(&StatsSection, Option<&Children>)>();
    let mut rows = world.query::<&StatsRow>();
    let mut layout = Vec::new();
    for children in roots.iter(world) {
        for &entity in children {
            let Ok((section, children)) = sections.get(world, entity) else {
                continue;
            };
            let mut section_rows: Vec<RowLayout> = children
                .into_iter()
                .flatten()
                .filter_map(|&row| {
                    let stats_row = rows.get(world, row).ok()?;
                    Some(RowLayout {
                        entity: row,
                        order: stats_row.order,
                        label: stats_row.label.clone(),
                    })
                })
                .collect();
            section_rows.sort_by_key(|row| row.order);
            layout.push(SectionLayout {
                entity,
                order: section.order,
                title: section.title.clone(),
                header: section.header.clone(),
                columns: section.value_columns(),
                rows: section_rows,
            });
        }
    }
    layout.sort_by_key(|section| section.order);
    let layout = Layout(layout);
    warn_unplaced(world, &layout, warnings);
    layout
}

fn warn_unplaced(world: &mut World, layout: &Layout, warnings: &mut StatsWarnings) {
    let placed_sections: HashSet<Entity> = layout.0.iter().map(|section| section.entity).collect();
    let placed_rows: HashSet<Entity> = layout
        .0
        .iter()
        .flat_map(|section| section.rows.iter().map(|row| row.entity))
        .collect();
    let mut sections = world.query_filtered::<(Entity, &Name), With<StatsSection>>();
    for (entity, name) in sections.iter(world) {
        if !placed_sections.contains(&entity) && warnings.first(entity, StatsWarning::Unplaced) {
            warn!(
                "Statistics section {name} ({entity}) is not a child of the statistics content root; not shown"
            );
        }
    }
    let mut rows = world.query::<(Entity, &StatsRow)>();
    for (entity, row) in rows.iter(world) {
        if !placed_rows.contains(&entity) && warnings.first(entity, StatsWarning::Unplaced) {
            warn!(
                "Statistics row {:?} ({entity}) is not in a shown StatsSection; not shown",
                row.label
            );
        }
    }
}

/// The cells of every row the panel of `host` shows.
pub(crate) fn values(
    world: &mut World,
    layout: &Layout,
    host: Entity,
    warnings: &mut StatsWarnings,
) -> StatsPanelValues {
    let mut values = StatsPanelValues::default();
    for section in &layout.0 {
        for row in &section.rows {
            let Some(cells) = row_cells(world, row, host, warnings) else {
                continue;
            };
            let (cells, fitted) = fit_cells(cells.0, section.columns);
            if !fitted && warnings.first(row.entity, StatsWarning::Cells) {
                warn!(
                    "Statistics row {:?} ({}) returned a cell count other than its section's {} columns; joined into the last",
                    row.label, row.entity, section.columns
                );
            }
            values.0.insert(row.entity, cells);
        }
    }
    values
}

/// Exactly `columns` cells; a different count is joined into the last. False when it did not fit.
pub(crate) fn fit_cells(cells: Vec<String>, columns: usize) -> (Vec<String>, bool) {
    if cells.len() == columns {
        return (cells, true);
    }
    let mut fitted = vec![String::new(); columns.saturating_sub(1)];
    fitted.push(cells.join(" "));
    (fitted, false)
}

fn row_cells(
    world: &mut World,
    row: &RowLayout,
    host: Entity,
    warnings: &mut StatsWarnings,
) -> Option<StatsCells> {
    let stats_row = world.get::<StatsRow>(row.entity)?;
    if let StatsRowSource::Diagnostic(path) = &stats_row.source {
        return diagnostic_cells(world, path);
    }
    let id = row_system(world, row.entity)?;
    match world.run_system_with(id, host) {
        Ok(cells) => cells,
        // Not an error: a parameter such as `Single` found nothing.
        Err(RegisteredSystemError::Skipped(_)) => None,
        Err(error) => {
            if warnings.first(row.entity, StatsWarning::Failed) {
                warn!(
                    "Statistics row {:?} ({}) is hidden: {error}",
                    row.label, row.entity
                );
            }
            None
        }
    }
}

fn diagnostic_cells(world: &World, path: &DiagnosticPath) -> Option<StatsCells> {
    let diagnostic = world.get_resource::<DiagnosticsStore>()?.get(path)?;
    if !diagnostic.is_enabled {
        return None;
    }
    let value = diagnostic.value()?;
    let text = if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    };
    Some(StatsCells::one(format!("{text}{}", diagnostic.suffix)))
}

/// The row's system id, registering the system on first use. `None` for a diagnostic row.
fn row_system(world: &mut World, row: Entity) -> Option<SystemId<In<Entity>, Option<StatsCells>>> {
    let pending = {
        let mut stats_row = world.get_mut::<StatsRow>(row)?;
        let StatsRowSource::System(system) = &mut stats_row.bypass_change_detection().source else {
            return None;
        };
        match &mut system.0 {
            RowSystem::Registered(handle) => return Some(SystemId::from(&*handle)),
            RowSystem::Pending(pending) => pending.take()?,
        }
    };
    let handle = world.register_tracked_boxed_system(pending);
    let id = SystemId::from(&handle);
    let mut stats_row = world.get_mut::<StatsRow>(row)?;
    if let StatsRowSource::System(system) = &mut stats_row.bypass_change_detection().source {
        system.0 = RowSystem::Registered(handle);
    }
    Some(id)
}
