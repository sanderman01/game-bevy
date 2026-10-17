# Known crashes

Crashes the engine is known to have, so an agent that hits one gets told what it is instead of
investigating it again.

When a game panics, the MCP sidecar matches the panic message and location (or, for a game that
died without a panic, the last error it logged) against the `Signature:` line of every entry
below, and links the first match: `docs/known-crashes.md#<heading slug>`.

Format of an entry:

- A `##` heading naming the crash. Its GitHub slug is the link's anchor.
- A `Signature:` line holding, in backticks, a literal substring of the panic message or its
  `file:line` location. Pick text that is specific to this crash and survives a rebuild: a
  message rather than a line number.
- Then what triggers it, what to do instead, and an issue link if there is one.

The sidecar reads `ENAME_KNOWN_CRASHES`, a path list (`:` on Unix, `;` on Windows) relative to
its working directory, defaulting to this file. A game built on the engine lists its own file and
this one.

## Sun shadow maps toggled back on

Signature: `Failed to get directional light visible entities for cascade`

Setting `DirectionalLight::shadow_maps_enabled` to `false` and then back to `true` on a running
game panics in Bevy's render world (`bevy_pbr` `render/light.rs`, `specialize_shadows`) and the
game exits with code 1. Reproduced on the example stage's light with two
`world_mutate_component` calls. Leave shadows on, or restart the game after turning them back on
in the stage file instead.
