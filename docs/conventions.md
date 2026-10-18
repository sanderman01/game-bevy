# Conventions

## Coordinate System

Like the most of the bevy ecosystem, this project uses a right-handed Y-up coordinate
system for the game world.

- +X points right
- +Y points up
- -Z points forward, +Z points rearward
- camera: +Z points towards you, out of the screen.

## Window pixels

Agent tools give screen points in physical window pixels, origin top-left: the pixels of a
`screenshot_capture` `window` PNG. `input_send` also takes `"in": "editor_camera"` or
`"game_camera"` for pixels of that source's full-size PNG. Bevy's `CursorMoved` and
`PointerLocation` are logical pixels (physical divided by the window's scale factor); egui works in
points (logical divided by its zoom). Conversions between them live in
`ename_editor/src/input_inject/geometry.rs`.

## Assets

Content assets should have an alias assigned to them in a corresponding .alias
file or in _alias_rules.toml.

eg: 
- assets/myasset.txt
- assets/myasset.meta
- assets/myasset.alias

when moving an asset, their corresponding .meta and .alias should be moved with it.

## Per-user directories

`ename_engine::application::ApplicationPaths` (a resource) gives each project two directories, laid
out by the convention of the OS it runs on:

| OS | Persistent data | Cache |
| --- | --- | --- |
| Linux | `$XDG_DATA_HOME/<project>`, else `~/.local/share/<project>` | `$XDG_CACHE_HOME/<project>`, else `~/.cache/<project>` |
| Windows | `%LOCALAPPDATA%\<org>\<project>\data` | `%LOCALAPPDATA%\<org>\<project>\cache` |
| macOS | `~/Library/Application Support/com.<org>.<project>` | `~/Library/Caches/com.<org>.<project>` |

- Persistent data is for anything the player would miss: saves, settings, the load order file.
- The cache is for anything that can be regenerated. The user or the OS may delete it at any
  time, so call `ensure_cache_path` before each use.
- `ApplicationPlugin` creates both directories when the app is built, before any `Startup`
  system. A failure is logged as a warning, not fatal.
- The binary passes both names to `EnginePlugins::new(organisation, project)`; there is no
  default. The example game uses `ename` and
  `example-game`.
- The names are storage identifiers, not display names: changing one makes existing saves look
  lost. They allow ASCII letters, digits, `-` and spaces, starting and ending with a letter or
  digit. On macOS they are lowercased with spaces removed (`com.alexanderverbeek.airshipgame`),
  so `My Game` and `MyGame` share a macOS folder.
  Names differing only by case share a folder on Windows and macOS.
- A relative base path, or one containing `..`, counts as no directory. A cache that overlaps the
  persistent data directory is dropped with a warning, so clearing it does not delete saves. The
  check compares paths as written (case-insensitively on Windows and macOS), so a symlink can
  still alias the two.
