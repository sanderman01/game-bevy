# Save sections

Part of the [design record](design.md). The module is `ename_engine::save`; the wall clock it uses
for save times is `ename_engine::clock`.

A game saves its state as named sections. Each section is owned by one system, has its own
version, and is stored as one raw RON value. The engine holds the registry and the container; the
sections, and where their bytes go (a record in [user storage](user-storage.md), say), are the
game's.

## Registering

```rust
app.register_save_section(
    SaveSection::new("weather", 2, capture_weather, apply_weather)
        .migration(1, weather_1_to_2),
);
```

| Part | Type | Does |
| --- | --- | --- |
| `capture` | `fn(&mut World) -> Option<T>` | Reads the world into the section's save struct. `None` saves no section |
| `apply` | `fn(&mut World, T, &mut Vec<String>)` | Puts a section back; may add lines to a report for the player |
| `migration(from, f)` | `fn(ron::Value) -> Result<ron::Value, String>` | Turns version `from` into `from + 1` |

- `T` is a hand-written save struct with `Serialize` and `Deserialize`, not a live component: a
  save outlives field renames, and transient state, entity ids and grid cells never reach it.
- Versions start at 1. A name registers once; a duplicate panics.
- Sections apply in registration order.

## Container

`SavedSections` is what a game stores, format 1:

```ron
(
  format: 1,
  sections: {
    "spawning": (version: 1, value: (rng: (state: 7, inc: 9), disarmed: [], due: [])),
    "weather": (version: 2, value: (wind: (3.0, 0.0, 1.0), temperature_offset: 0.0)),
  },
)
```

- Each value is one line of RON. `to_ron` writes the container, `from_ron` reads it and refuses a
  missing or newer `format`.
- The whitespace before a raw value is trimmed on read, so a section kept and written back stays
  the same text.

## Saving

1. `capture_sections(world)` runs every capture in one go, in one frame, so the sections agree.
   It only clones state into save structs.
2. `CapturedSections::serialize()` turns them into RON. It holds no world, so it runs on a task.
3. `SavedSections::keep(&unknown)` adds back sections an earlier load did not know.

## Loading

1. `check_sections(world, &saved)` reads every known section into its struct without touching the
   world. It fails if any section fails, so a game can refuse a save before it unloads anything.
2. `CheckedSections::get::<T>(name)` reads a section before applying, such as what a session needs
   to start.
3. `apply_sections(world, checked, &mut report)` applies them in registration order.

| Case | Result |
| --- | --- |
| Version as registered | Read leniently: unknown fields are skipped and listed in `skipped` as `name: path`; missing ones take the struct's defaults |
| Older version | Its migrations run in order on the raw value, then it is read leniently |
| Older version with a migration missing | `NoMigration` |
| A migration fails | `Migration` |
| Newer version, or a newer container format | `NewerVersion`, `NewerFormat` |
| A value that does not parse | `Invalid` |
| A section no system registers | Kept in `unknown`, verbatim, for a later `keep` |

`SectionError::kind()` gives a stable name for each: `newer_version`, `migration_failed`,
`unreadable`, `serialize_failed`.

## Wall clock

`clock::now_unix()` is whole seconds since the Unix epoch, through `web-time`, so it works in the
browser, where `std::time::SystemTime::now` panics.
