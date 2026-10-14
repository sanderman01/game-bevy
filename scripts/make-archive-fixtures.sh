#!/usr/bin/env bash
# Regenerates the archive regression fixtures in ename_asset_vfs/tests/fixtures/archives from the
# `basic/` source tree beside them. Needs Info-ZIP `zip` and GNU `tar`.
#
# Deterministic apart from the uid/gid Info-ZIP records: `basic.zip` keeps Info-ZIP's extra
# fields on purpose, because its local `UT` field is longer than the central one, which is the
# case the reader must resolve from the local header. The tests compare members and bytes, never
# archive bytes, so a regenerated fixture that differs only there is equivalent.
set -euo pipefail

dir="$(cd "$(dirname "$0")/../ename_asset_vfs/tests/fixtures/archives" && pwd)"
cd "$dir"
export TZ=UTC
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cp -r basic "$work/basic"
find "$work/basic" -exec touch -t 202001010000 {} +
# Files only, sorted: no directory entries, and the same order every time.
files() { (cd "$1" && find . -type f | sed 's|^\./||' | LC_ALL=C sort); }

rm -f basic.zip basic.tar zip64.zip macosx.zip bad/*.zip
mkdir -p bad

# Mixed Stored and Deflate: `.ron` is stored, the rest deflated (Info-ZIP stores a file that
# does not shrink anyway).
(cd "$work/basic" && files . | zip -q -D -n .ron -@ "$dir/basic.zip")
(cd "$work/basic" && files . | zip -q -X -D -fz -@ "$dir/zip64.zip")
files "$work/basic" | tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
    --format=ustar --no-recursion -C "$work/basic" -T - -cf basic.tar

mkdir -p "$work/mac/__MACOSX"
echo "resource fork" > "$work/mac/__MACOSX/._a.txt"
echo "first" > "$work/mac/a.txt"
find "$work/mac" -exec touch -t 202001010000 {} +
(cd "$work/mac" && files . | zip -q -X -D -@ "$dir/macosx.zip")

# Cut short through the central directory.
head -c "$(($(stat -c %s basic.zip) - 40))" basic.zip > bad/truncated.zip
# `a.txt` is stored, so its bytes appear verbatim: change one and its CRC no longer matches.
cp basic.zip bad/crc.zip
offset="$(grep -obUa 'first' bad/crc.zip | head -1 | cut -d: -f1)"
printf 'F' | dd of=bad/crc.zip bs=1 seek="$offset" conv=notrunc status=none

ls -l basic.zip basic.tar zip64.zip macosx.zip bad
