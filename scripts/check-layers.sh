#!/usr/bin/env bash
# Fails the build when a crate depends on a layer above it.
# The graph this enforces is in docs/design/crate-layout.md.
set -euo pipefail

fail=0

# check <crate> <forbidden crates, regex alternation>
check() {
    local crate="$1" forbidden="$2" deps found
    # `--no-default-features` so this tests the shipping graph: the editor is an optional
    # dependency of the `example_game_editor_bin` binary and must be absent from it with the
    # feature off.
    # The tree is captured before the grep runs: with both in one pipeline, the `|| true`
    # for grep's legitimate no-match exit would also swallow a `cargo tree` failure.
    deps=$(cargo tree -p "$crate" -e normal --no-default-features --prefix none \
        | awk 'NR > 1 { print $1 }' \
        | sort -u)
    found=$(printf '%s\n' "$deps" | grep -xE "$forbidden" || true)
    if [ -n "$found" ]; then
        echo "layer violation: $crate depends on:" >&2
        echo "$found" | sed 's/^/  /' >&2
        fail=1
    fi
}

# `ename_engine` may depend on the asset crates below it, and must not name anything above.
check ename_engine 'ename_editor|example_game_lib'
# The bottom of the asset stack. `ename_asset_alias` is the leaf: it owns the alias, the
# `.alias` and `_alias_rules.toml` schema, and the walk over them, and names nothing first-party.
check ename_asset_alias 'ename_.*'
# `ename_asset_package` owns packages, load order, and the fold that turns an ordered scan into
# one index, all on top of that same walk, so the alias leaf is the only first-party crate it
# may name.
check ename_asset_package 'ename_asset_content|ename_engine|ename_editor|example_game_lib|ename_remote|ename_mcp'
# The glue may name both asset crates and nothing above it.
check ename_asset_content 'ename_engine|ename_editor|example_game_lib'
check ename_editor 'example_game_lib'
check example_game_lib 'ename_editor'
check ename_remote 'ename_editor|example_game_lib'
# The sidecar is a separate process. A first-party dependency here would mean it had started
# linking the engine it is supposed to talk to over a socket.
check ename_mcp 'ename_.*'
# The tooling binary. It reads the asset crates with StdVfs and must never link the renderer,
# the editor, the game, or `ename_asset_content` (which always links Bevy) -- if it did,
# `cargo xtask ename_check` would rebuild half the engine.
check ename_xtask 'ename_asset_content|ename_engine|ename_editor|example_game_lib|ename_remote|ename_mcp'
# `--no-default-features` also proves the `agent` feature is off in the shipping graph, which
# matters more than the editor: `ename_remote` is unauthenticated write access to the world.
check example_game_editor_bin 'ename_editor|ename_remote'

exit "$fail"
