#!/usr/bin/env bash
# Build Hydroxl's QEMU runtime and signed Windows driver media.
set -euo pipefail

if (( $# < 2 || $# > 4 )); then
    echo "Usage: $0 ABSOLUTE_PREFIX WORK_DIR [DESTDIR [ARCHIVE_CACHE]]" >&2
    exit 2
fi

prefix=$1
work=$(realpath -m "$2")
destdir=${3:-}
cache=$(realpath -m "${4:-$work/archives}")
if [[ "$prefix" != /* || ( -n "$destdir" && "$destdir" != /* ) ]]; then
    echo "Use absolute paths for the install prefix and DESTDIR." >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
jobs=${HYDROXL_BUILD_JOBS:-$(nproc)}
mkdir -p "$work" "$cache"
exec 9>"$work/.build.lock"
flock 9

key=$({ sha256sum "$repo/scripts/neptune-sources.txt" "$repo/patches/neptune/qemu-11.1.3.patch" "$repo/scripts/build-neptune.sh"
        printf '%s\n' "$prefix"
    } | sha256sum | cut -c1-16)
build="$work/$key"
sources="$build/source"
stage="$destdir$prefix"
lib="$stage/lib"
stamp="$stage/.hydroxl-$key"
media="$stage/share/hydroxl"
media_key=$({ sha256sum "$repo"/scripts/triton/{build-iso.sh,INSTALL.TXT,Install-Triton.cmd,Install-Triton.ps1}
              sed -n '/^triton-/p' "$repo/scripts/neptune-sources.txt"
    } | sha256sum | cut -c1-16)
media_stamp="$media/.triton-build"

fetch_archives() {
    local pattern=$1 archive digest url part
    while read -r archive digest url; do
        [[ -n "$archive" && "$archive" != \#* && "$archive" == $pattern ]] || continue
        if [[ ! -f "$cache/$archive" ]]; then
            (
                part=$(mktemp "$cache/$archive.XXXXXXXX")
                trap 'rm -f -- "$part"' EXIT
                curl --fail --location --retry 3 --proto '=https' --proto-redir '=https' \
                    --output "$part" "$url"
                sha256sum --check --status <<< "$digest  $part"
                mv -f -- "$part" "$cache/$archive"
            )
        fi
        sha256sum --check <<< "$digest  $cache/$archive"
    done < "$repo/scripts/neptune-sources.txt"
}

build_driver_media() {
    if [[ -f "$media_stamp" && "$(< "$media_stamp")" == "$media_key" &&
          -f "$media/triton-x86_64.iso" && -f "$media/triton-aarch64.iso" ]]; then
        return
    fi
    fetch_archives 'triton-*'
    bash "$repo/scripts/triton/build-iso.sh" "$cache" "$media"
    printf '%s\n' "$media_key" > "$media_stamp"
}

if [[ -f "$stamp" && -x "$stage/bin/qemu-img" && -x "$stage/bin/qemu-system-x86_64" ]]; then
    build_driver_media
    exit 0
fi

fetch_archives '*'

mkdir -p "$sources"

unpack() {
    local archive=$1 destination="$sources/$2"
    mkdir -p "$destination"
    tar -xf "$cache/$archive" --strip-components=1 -C "$destination"
}
if [[ ! -f "$sources/.prepared" ]]; then
    unpack qemu-release.tar.xz qemu
    unpack virgl-nov-src.tar.gz virglrenderer
    unpack dxvk-src.tar.gz dxvk
    unpack vkd3d-src.tar.gz vkd3d
    unpack directx-native-nov-src.tar.gz virglrenderer/subprojects/directx-native
    unpack directx-native-src.tar.gz dxvk/subprojects/directx-native
    unpack directx-native-src.tar.gz vkd3d/subprojects/directx-native
    unpack dxvk-directx-src.tar.gz dxvk/include/native/directx
    unpack dxvk-spirv-src.tar.gz dxvk/include/spirv
    unpack dxvk-vulkan-src.tar.gz dxvk/include/vulkan
    unpack display-info-src.tar.gz dxvk/subprojects/libdisplay-info
    unpack dxbc-spirv-src.tar.gz dxvk/subprojects/dxbc-spirv
    unpack dxbc-spirv-headers.tar.gz dxvk/subprojects/dxbc-spirv/submodules/spirv_headers
    unpack vkd3d-spirv-src.tar.gz vkd3d/khronos/SPIRV-Headers
    unpack vkd3d-vulkan-src.tar.gz vkd3d/khronos/Vulkan-Headers
    unpack dxil-spirv-src.tar.gz vkd3d/subprojects/dxil-spirv
    unpack dxil-spirv-headers.tar.gz vkd3d/subprojects/dxil-spirv/third_party/spirv-headers
    unpack dxil-dxbc-src.tar.gz vkd3d/subprojects/dxil-spirv/subprojects/dxbc-spirv
    unpack dxbc-spirv-headers.tar.gz vkd3d/subprojects/dxil-spirv/subprojects/dxbc-spirv/submodules/spirv_headers
    patch --directory "$sources/qemu" -p1 < "$repo/patches/neptune/qemu-11.1.3.patch"
    touch "$sources/.prepared"
fi

meson_build() {
    local project=$1
    shift
    local directory="$build/$project"
    local -a reconfigure=()
    if [[ -f "$directory/meson-private/coredata.dat" ]]; then
        reconfigure=(--reconfigure)
    fi
    meson setup "${reconfigure[@]}" "$directory" "$sources/$project" --wrap-mode=nodownload \
        --prefix="$prefix" --libdir=lib --libexecdir=libexec --buildtype=release \
        -Dpkgconfig.relocatable=true "$@"
    ninja -C "$directory" -j "$jobs"
    DESTDIR="$destdir" meson install --no-rebuild -C "$directory"
}

# virglrenderer loads these libraries with dlopen in its isolated render worker.
rpath="-Wl,-rpath,$prefix/lib"
meson_build dxvk -Dnative_headless=true -Dnative_sdl2=disabled -Dnative_sdl3=disabled \
    -Dnative_glfw=disabled -Db_ndebug=true -Dcpp_args=-U_GLIBCXX_ASSERTIONS \
    -Dc_args=-U_GLIBCXX_ASSERTIONS "-Dcpp_link_args=$rpath"
meson_build vkd3d "-Dc_link_args=$rpath" "-Dcpp_link_args=$rpath"
# GCC 14 rejects a declaration after a label in vrend_renderer.c under -Werror=pedantic.
meson_build virglrenderer -Dneptune=true -Dvenus=true -Dtests=false -Dvtest=false \
    -Dc_args=-Wno-error=pedantic "-Dc_link_args=$rpath"
ln -sfn libdxvk_d3d11.so "$lib/libd3d11.so"
ln -sfn libdxvk_dxgi.so "$lib/libdxgi.so"

# Use the freshly staged renderer when QEMU probes dependencies during its build.
export PKG_CONFIG_PATH="$lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
export LD_LIBRARY_PATH="$lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
mkdir -p "$build/qemu"
(
    cd "$build/qemu"
    "$sources/qemu/configure" --prefix="$prefix" --libdir=lib \
        --target-list=x86_64-softmmu,aarch64-softmmu,arm-softmmu,riscv64-softmmu,ppc-softmmu,ppc64-softmmu \
        --without-default-features --disable-download --disable-docs --disable-debug-info --enable-system \
        --enable-tools --enable-kvm --enable-tcg --enable-fdt=system \
        --enable-pixman --enable-gio --enable-tpm --enable-passt \
        --enable-opengl --enable-virglrenderer --enable-spice --enable-dbus-display \
        --enable-slirp --enable-usb-redir --enable-libusb --enable-libudev --enable-vnc \
        --enable-vhost-user --enable-vhost-kernel --enable-vhost-net --enable-virtfs \
        --enable-attr --enable-cap-ng \
        --enable-alsa --enable-pa --enable-pipewire --audio-drv-list=alsa,pa,pipewire \
        --enable-seccomp --enable-stack-protector --enable-png --enable-gnutls \
        --enable-zstd --enable-lzo --enable-linux-aio --enable-linux-io-uring \
        --enable-vdi --enable-vmdk --enable-vhdx --enable-numa --enable-curl \
        "--extra-ldflags=$rpath"
    DESTDIR="$destdir" ninja -j "$jobs" install
)

build_driver_media

licenses="$media/licenses"
install -Dm644 "$sources/qemu/COPYING" "$licenses/QEMU-COPYING"
install -Dm644 "$sources/qemu/COPYING.LIB" "$licenses/QEMU-COPYING.LIB"
install -Dm644 "$sources/virglrenderer/COPYING" "$licenses/virglrenderer-COPYING"
install -Dm644 "$sources/dxvk/LICENSE" "$licenses/DXVK-LICENSE"
install -Dm644 "$sources/vkd3d/COPYING" "$licenses/vkd3d-proton-COPYING"

for architecture in x86_64 aarch64; do
    device=virtio-gpu-gl-pci
    [[ "$architecture" != x86_64 ]] || device=virtio-vga-gl
    "$stage/bin/qemu-system-$architecture" -device "$device,help" |
        grep -E '^[[:space:]]+neptune='
done
"$stage/bin/qemu-img" --version
touch "$stamp"
