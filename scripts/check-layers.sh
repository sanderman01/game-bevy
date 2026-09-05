#!/usr/bin/env bash
# Fails the build when a crate depends on a layer above it.
# The graph this enforces is in docs/design.md.
set -euo pipefail

fail=0

# check <crate> <forbidden crates, regex alternation>
check() {
    local crate="$1" forbidden="$2" deps found
    # `--no-default-features` so this tests the shipping graph: the editor is an optional
    # dependency of `game` and must be absent from it with the feature off.
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

check engine 'editor|modloader|game'
check modloader 'engine|editor|game'
check editor 'game|modloader'
check game 'editor'

exit "$fail"
