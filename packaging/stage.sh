#!/usr/bin/env bash
# Install Hydroxl's files into DESTDIR. Shared by the Arch, Debian and RPM packages
# so all three ship the same layout.
set -euo pipefail

if (( $# != 3 )); then
    echo "Usage: $0 HYDROXL_BINARY NEPTUNE_DESTDIR DESTDIR" >&2
    exit 2
fi

binary=$1
neptune=$2
destdir=$3
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
sharedir="$destdir/usr/share"
licensedir="$sharedir/licenses/hydroxl"

install -Dm755 "$binary" "$destdir/usr/bin/hydroxl"
install -Dm644 -t "$sharedir/applications" "$repo/packaging/dev.hydroxl.Hydroxl.desktop"
install -Dm644 -t "$sharedir/icons/hicolor/scalable/apps" "$repo/packaging/dev.hydroxl.Hydroxl.svg"
install -Dm644 -t "$licensedir" "$repo/LICENSE" "$repo/vendor/LICENSE-APACHE" \
    "$repo/assets/fonts/Geist-OFL.txt"
install -Dm644 "$repo/assets/os/LICENSE.md" "$licensedir/OS-LOGOS.md"

# The bundled Neptune QEMU, built for /usr/lib/hydroxl/neptune.
install -d "$destdir/usr/lib/hydroxl" "$sharedir/qemu/firmware"
cp -a "$neptune/usr/lib/hydroxl/neptune" "$destdir/usr/lib/hydroxl/"

# libvirt discovers firmware through these descriptors; prefix them so they never
# collide with the distribution's own.
for descriptor in "$destdir/usr/lib/hydroxl/neptune/share/qemu/firmware/"*.json; do
    [[ -e "$descriptor" ]] || continue
    filename=${descriptor##*/}
    install -m644 "$descriptor" "$sharedir/qemu/firmware/${filename%%-*}-hydroxl-${filename#*-}"
done
