# ename

Bevy and the kitchen sink.

`ename` is a game engine built as a set of Rust crates on top of
[Bevy](https://bevyengine.org). Bevy gives you an ECS, a renderer and a plugin system,
and stops there on purpose. This fills in the layer above it. An integrated and opinionated pile
covering:

- An editor (`egui` for now)
- In-game log event buffer and console with filtering
- Asset naming, referencing, and packaging
- `big_space` floating origin
- `avian3d` physics
- A BRP socket and an MCP sidecar, so a coding agent can inspect and drive the running game.

A game project depends on the crates it wants and starts writing gameplay."

## Status

Work in progress. Every crate is at version `0.0.0`, nothing is published, and there is no
stability promise of any kind. Names, APIs and on-disk file formats change without notice.

Codenamed `ename` until I decide the actual name.

Requires bevy = "0.19".

## The crates

| Crate | What it does |
| --- | --- |
| `ename` | The binary. Decides which layers link in. |
| `ename_engine` | Runtime layer: camera rig, `big_space` floating origin, Avian physics glue, input, time control, log capture. |
| `ename_editor` | egui editor: dock layout, selection, viewport, console, gizmos. Game-agnostic. |
| `ename_game`, `ename_game_editor` | Where game and game-specific tooling go. |
| `ename_asset_alias` | Addressing an asset by name. `asset_server.load("alias://core::airship")` instead of a path. |
| `ename_asset_package` | Content packages: manifests, versions, load order, overrides. |
| `ename_asset_content` | Where the previous two meet, plus the plugin that installs both. |
| `ename_remote` | BRP server and custom methods for agent access. (Development only, never ship it.) |
| `ename_mcp` | MCP sidecar process. Lets a coding agent read and change the running game's world. |
| `ename_xtask` | Command line content pipeline tooling. |

Dependencies point down only. The editor never links into a shipping build.

## Docs

- [docs/design.md](docs/design.md) -- decisions and why.
- [docs/code-style.md](docs/code-style.md) -- how the Rust is written.
- [docs/conventions.md](docs/conventions.md) -- axes, units, socket names, import settings.
- [docs/agent-tools.md](docs/agent-tools.md) -- driving the running game from a coding agent.

## Contributing

This project is not accepting issues or pull requests at this time. It is one person's project and
its shape changes faster than a review cycle could keep up with, so an open queue would waste your
time and mine. That may change once the APIs stop moving.

Reading the code, forking it and taking pieces for your own project are all fine. Both licenses
below allow it.

## License

MIT or Apache-2.0, at your option. See [LICENSE-MIT.txt](LICENSE-MIT.txt) and
[LICENSE-APACHE.txt](LICENSE-APACHE.txt).
