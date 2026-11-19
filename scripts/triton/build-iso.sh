#!/usr/bin/env bash
# Keep the signed driver files intact and add Hydroxl's setup files alongside them.
set -euo pipefail

assets=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cache=$1
media=$2
mkdir -p "$media"
scratch=$(mktemp -d "$media/.triton.XXXXXXXX")
trap 'rm -rf -- "$scratch"' EXIT

triton_iso() {
    local architecture=$1 archive=$2 volume=$3 disk="$scratch/$1"
    mkdir "$disk"
    unzip -oq "$cache/$archive" -d "$disk"
    test -f "$disk/drivers/viogpu3d/viogpu3d.inf"
    for file in INSTALL.TXT Install-Triton.cmd Install-Triton.ps1; do
        sed 's/$/\r/' "$assets/$file" > "$disk/$file"
    done
    xorriso -as mkisofs -quiet -J -r -V "$volume" -o "$scratch/triton-$architecture.iso" "$disk"
}

triton_iso x86_64 triton-v06-x64.zip TRITON_X64
triton_iso aarch64 triton-v06-arm64.zip TRITON_ARM64
mv -f -- "$scratch/triton-x86_64.iso" "$scratch/triton-aarch64.iso" "$media"
