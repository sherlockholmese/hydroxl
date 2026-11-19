#!/usr/bin/env bash
# Build Hydroxl and its Neptune bundle for /usr, then stage the install tree.
# Used for the Debian and RPM packages; the Arch package uses makepkg.
set -euo pipefail

if (( $# != 1 )); then
    echo "Usage: $0 STAGE_DIR" >&2
    exit 2
fi

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
stage=$(realpath -m "$1")
work="$repo/target/package"
neptune="$work/neptune-stage"
rm -rf -- "$stage" "$neptune"
mkdir -p "$work"

cd "$repo"
HYDROXL_QEMU_BUILD_PREFIX=/usr/lib/hydroxl/neptune \
    HYDROXL_QEMU_DESTDIR="$neptune" \
    HYDROXL_QEMU_ARCHIVE_CACHE="$work/archives" \
    cargo build --release --locked \
    --config "build.rustflags=['--remap-path-prefix=$repo=/usr/src/hydroxl']"

"$repo/packaging/stage.sh" "$repo/target/release/hydroxl" "$neptune" "$stage"
