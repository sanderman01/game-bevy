# Code style

How Rust and Bevy code is written in this project. Read it before writing or reviewing code.

This document covers judgment calls that tooling cannot catch. `cargo fmt` and `cargo clippy`
own everything they can decide themselves, and they always win. Nothing here restates a rule
an experienced Rust programmer already follows.

`docs/bevy-best-practices.md` is separate background reading on entity hygiene, state-scoped
cleanup, preludes, and build profiles. Its patterns still hold, but it was written against
Bevy 0.11 to 0.14 and many of its API names are dead. Read the outdated-API table at the top
of that file before copying any code out of it.

Bevy is pinned at 0.19. Where a rule names an API symbol, the concept is the rule. If a symbol
here no longer exists after an upgrade, fix this document in the same commit as the upgrade.

## Crate layers

`crates/` holds layers, not features. Dependencies point one direction and there are no cycles.

The graph itself is in [design/crate-layout.md](design/crate-layout.md#layer-graph). Keep one copy;
this file only states the rules that follow from it.

`ename_engine` never depends on `ename_game` or `ename_editor`. It does depend on
`ename_asset_content`, because the `alias://` source has to be registered before `AssetPlugin`
builds and `EnginePlugins` owns `AssetPlugin`; the asset crates are a layer below the engine, not
beside it. If engine code needs something from a higher layer, the design is wrong: move the
shared type down into `ename_engine` or invert the call into an event the higher layer observes.

`scripts/check-layers.sh` enforces this in CI. Record any change to the layer graph in
`docs/design/crate-layout.md` before making it.

## Features are modules until they earn a crate

A feature starts as a module inside the layer crate that owns it. For example
`crates/ename_engine/src/physics/`.

Promote it to its own crate only when one of these is true:

- A second crate needs to depend on it directly.
- It measurably slows incremental builds.

Nothing else justifies a new crate. Not size, not tidiness, not anticipated reuse.

## What a feature module exposes

One plugin per feature module. The plugin is the only place the feature touches `App`.
Registration of its systems, resources, observers, and third-party plugins all happen there
and nowhere else.

A feature module's public surface is the plugin, the components and events other features
legitimately need, and nothing more. Systems are private. Helper functions are private.
Resources are private unless another feature must read them, and then prefer exposing a
component or an event instead.

Split a feature module by behaviour, never by technical layer. `orbit.rs` and `input.rs`,
not `systems.rs` and `components.rs`. A file named after a Bevy concept is a sign the split
carries no meaning.

## Data modeling

Choose the storage that matches the data's lifetime and cardinality.

- Per-entity state is a component.
- Global, single-instance state is a resource. If two of something could ever exist, it is
  not a resource.
- A link between entities is a relationship, not an `Entity` field you maintain by hand.
  Hand-rolled `Entity` fields go stale on despawn and nothing tells you.

Keep components small and single-purpose. One component per independent piece of state, so
change detection and queries stay precise. A component with six unrelated fields forces every
system to depend on all six.

An `Option` field in a component usually means two states that should be two components. Add
and remove the component instead of nulling the field. The exception is when the absent case is
genuinely part of the same value, such as an optional tuning override.

Use marker components for classification, and query them with `With` and `Without` rather than
storing a kind enum and branching inside the system. Reach for an enum only when the variants
must be iterated or serialized as data.

Use required components to express "this cannot exist without that". It puts the invariant in
one place instead of in every spawn call.

Newtype any value with a unit or a coordinate space. `Meters(f32)` and `GridCell` prevent the
class of bug where a local offset is used as a world position. `docs/conventions.md` is the
authority on which units and spaces exist.

## Queries

Query the narrowest thing that works. Ask for `&T` when you do not write, and name only the
components the system actually uses. A wide query blocks parallel scheduling and hides what
the system really depends on.

Filter in the query, not in the body. `With`, `Without`, `Changed`, and `Added` push the work
into the ECS and let Bevy skip the system entirely when nothing matches. An `if` at the top of
a loop that skips most entities is a filter in the wrong place.

Express cardinality in the signature with `Single` and `Populated` rather than checking it in
the body. See Failure below for what they do when the expectation does not hold.

## Change detection

Any system that reacts to state rather than driving it takes `Changed<T>`. Recomputing every
frame because it is cheap today is how frame time rots.

Do not write a component unconditionally. Assigning an equal value still marks it changed and
wakes every downstream system. Compare first, then write.

Change detection is not a message queue. It tells you the current value differs, not how many
times it changed or in what order. If you need the sequence, send events.

## Cross-feature communication

Default: the acting feature triggers an event, the reacting feature observes it. Immediate,
no frame delay, and no ordering question to get wrong.

Buffered messages are for work that batches naturally or must be drained on a schedule, such
as input accumulated over a frame. Choosing buffered means accepting an ordering constraint, so
say in the plugin why the delay is correct.

Reading another feature's components directly is allowed only downward through the layer graph,
and only for components that feature deliberately made public. A higher layer never reaches
back down by querying.

Name events for what happened, in the past tense. `DamageTaken`, not `TakeDamage` or
`DamageEvent`. The type is already an event and the suffix adds nothing.

## Scheduling

Ordering constraints belong to system sets. A set is a named place to hang ordering, so the
constraint survives systems being added, renamed, or split.

Never order against a system function from another module. `.after(other_module::some_system)`
couples you to a private implementation detail and breaks silently when that function is
renamed. Order against the set.

Each crate declares its sets and their relative order in one place, so the schedule can be
read without opening every plugin.

Every ordering constraint needs a reason a reader can check. If you cannot say what breaks
without it, delete it and find out.

## Failure

A failed entity or resource lookup returns early and leaves the frame intact. It does not
propagate as an error. This follows the getter-macro pattern in
`docs/bevy-best-practices.md`, updated for APIs that now express it in the signature.

Prefer the system parameter that encodes the expectation, because it needs no code in the body:

- `Single<T>` when exactly one entity should match. Bevy skips the system when the query
  matches zero or many.
- `Populated<T>` when the system has nothing to do on an empty set.

For a lookup inside a system body, return early at the top with `let Ok(x) = ... else { return }`
or the project's getter macro. Keep early returns at the top of the function, never buried in a
branch halfway down.

This default trades a loud failure for a quiet one, so it needs two guards.

When a missing lookup means a bug rather than a state you expect, panic in debug and return in
release. A missing camera during rendering is a broken invariant, and finding it on the dev
machine beats shipping a frame that silently does nothing. Say what was expected and what was
found.

Never let a silent return be the only handler for something a player would notice. If failing
means no sound plays or no damage lands, log at `warn` with the entity or asset identified, and
not inside a per-entity loop that could fire thousands of times.

Reserve `Result` and `?` for failures that are genuinely exceptional rather than absent: asset
loading, IO, parsing, and anything crossing a crate boundary. Bevy's error handler logs those
with the system name attached, which is what you want for a real error and overkill for a
missing entity.

`unwrap` and `expect` in a system that runs every frame are a review question every time. An
`unwrap` that can fail is a crash on a timer.

## Naming

Systems are verb phrases describing the action: `apply_thrust`, `sync_grid_cells`. No `system`
suffix. The registration site says it is a system.

Sets are noun phrases naming a phase of work, not the systems inside them.

Components are the thing or the property, not the behaviour: `Velocity`, `Orbiting`. A
component named after a verb usually wants to be a marker or an event.

Reuse Bevy's vocabulary. If Bevy calls it a cell, do not call it a tile. Divergent names for
the same concept cost more than they save.

## Comments and docs

Every crate has a crate-level doc comment saying what the crate is for and where it sits in
the layer graph. Every plugin has a doc comment saying what the feature does and what it
registers. Those two are required.

Individual items get doc comments only when something is not obvious from the signature.
The things worth writing down:

- Invariants a caller must uphold.
- Units and coordinate spaces, when the type does not already say.
- Why the code is this way, when the obvious approach does not work.
- Ordering or timing the type depends on.

Do not write a comment that restates the code. Do not write a doc comment that expands the
item name into a sentence. If the only honest comment is a paraphrase, the code is already
clear and the comment is noise to maintain.

Comment the workaround, not the mechanism. When something exists because of an upstream bug
or a version incompatibility, say which one and link it. That comment is the only record of
why the code cannot be simplified.

`TODO` needs an owner or an issue. Otherwise it is a wish.

## Abstraction

Write the specific thing first. Introduce a trait, a generic, or a builder when a second real
caller exists and the shape they share is known. Not before, and never on the argument that
one is coming.

Three concrete implementations that share nothing but a name are worse than three separate
functions. Duplication is visible and cheap to fix. The wrong abstraction is invisible and
gets built on.

Delete code you replace. No commenting it out, no `_old` suffix, no keeping it behind a flag
nobody sets. Git remembers.

Do not add configuration for a value nobody has needed to change. A constant in one place is
easier to find and change than a resource threaded through a plugin.

Wrap a third-party crate only when you are hiding a real incompatibility or a genuinely bad
API. A wrapper that forwards calls unchanged adds a file to read and a name to learn.

## Things not to write

A checklist for reviewing your own diff.

- Modules named `systems.rs`, `components.rs`, or `resources.rs`. Split by behaviour.
- A new crate for a feature that only one crate uses.
- Guards that re-check something a query filter or run condition already guaranteed.
- Nested `if let` ladders for lookups. One early return at the top, or `Single`/`Populated`.
- Comments restating the line above them, or section banners made of `//` characters.
- A trait with one implementor, or a generic parameter with one instantiation.
- `.after(some_function)` reaching into another module.
- A component holding unrelated fields because they were spawned together.
- Writing a component every frame with the value it already has.
- An event type ending in `Event`, or a system ending in `system`.
