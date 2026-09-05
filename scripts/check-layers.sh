#!/usr/bin/env bash
# Fails the build when a crate depends on a layer above it.
# The graph this enforces is in docs/design.md.
set -euo pipefail

fail=0

# check <crate> <forbidden crates, regex alternation>
check() {
    local crate="$1" forbidden="$2" found
    found=$(cargo tree -p "$crate" -e normal --prefix none \
        | awk 'NR > 1 { print $1 }' \
        | sort -u \
        | grep -xE "$forbidden" || true)
    if [ -n "$found" ]; then
        echo "layer violation: $crate depends on:" >&2
        echo "$found" | sed 's/^/  /' >&2
        fail=1
    fi
}

check engine 'editor|modloader|game'
check modloader 'engine|editor|game'
check editor 'game|modloader'

exit "$fail"
