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

## Textures

Textures have mipmaps, are block compressed where the GPU decodes BC, and are sampled trilinearly
with anisotropic filtering, unless a texture says otherwise for a reason, such as a data texture
read texel by texel.

- `EnginePlugins` sets `ename_engine::texture::default_sampler` (anisotropy 16) as the sampler of
  every image at `ImageSampler::Default` and of every glTF texture; a glTF file's wrap modes stay.
- `ename_engine::texture::TextureLoadPlugin` replaces Bevy's image loader for every image file.
  A *default texture* is one loaded to be drawn (`RENDER_WORLD`), as a single 2D RGBA8 image,
  with the default or an anisotropic sampler. The loader gives it a mip chain if the file has
  none, then encodes it as BC1 (opaque), BC3 (with alpha) or BC5 (a normal map, recognised by
  its texels) where the GPU decodes BC and its size is whole 4 x 4 blocks; else it stays RGBA8.
  The result goes in the disk cache (`engine/textures`), keyed by the file's bytes, so only the
  first load pays. A data texture opts out by loading with a sampler of its own without
  anisotropy, or with `MAIN_WORLD` usage only. Bevy warns once at startup that two image loaders
  share their extensions; the engine's, registered last, is the one used.
- Images embedded in a `.glb` are decoded by the glTF loader itself and get none of this; models
  reference their textures as files instead.
- A texture made at runtime builds its own mip chain and sets `mip_level_count`.
- A texture exported with a model carries its mip chain on disk (KTX2), so no mips are generated
  at load. Models reference shared textures by `alias://` URI rather than embedding them, so
  models share one copy.

## glTF colliders

A glTF node named `COL_<what>` with the extras `{"collider": "<shape>"}` becomes an avian
`Collider` on that node when its scene spawns (`ename_engine::physics`). Its meshes are
despawned, so it never renders. A stage places a glTF by alias with
`WorldAssetRoot: (Path("alias://<alias>#Scene0"))`.

| Shape | Fitted to |
| --- | --- |
| `box` | The mesh's local bounding box |
| `sphere` | The bounding box's largest half extent |
| `capsule`, `cylinder` | The bounding box, axis along local Y, radius the larger of the X and Z half extents |
| `convex_hull` | The mesh's vertices |
| `trimesh` | The mesh itself; static structures only |

The collider joins the nearest ancestor `RigidBody`, or is static without one. Parent a collider
node to the part it moves with. The node also gets `GltfColliderShape`, so tools can find it.

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
