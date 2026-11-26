//! Discover 3D VirtIO GPU properties on the emulator used to start a domain.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuFeatures {
    pub venus: bool,
    pub neptune: bool,
}

/// Size of the window guest blob resources are mapped through, as UTM sets it.
pub const HOSTMEM_BYTES: u64 = 8 << 30;

pub const NEPTUNE_HOST_MISSING: &str = "Select a Neptune-enabled QEMU build in Overview.";
pub const TRITON_ARCH_UNSUPPORTED: &str =
    "Triton is available for x86-64 and ARM64 Windows guests.";

pub fn triton_arch(arch: &str) -> bool {
    matches!(arch, "x86_64" | "aarch64")
}

fn device(arch: &str) -> &'static str {
    if matches!(arch, "x86_64" | "i686") {
        "virtio-vga-gl"
    } else {
        "virtio-gpu-gl-pci"
    }
}

pub(crate) fn parse(help: &str) -> GpuFeatures {
    let has =
        |name: &str| help.lines().any(|line| line.trim_start().starts_with(&format!("{name}=")));
    GpuFeatures {
        venus: has("venus"),
        neptune: has("neptune"),
    }
}

pub fn gpu_features(arch: &str, emulator: Option<&str>) -> GpuFeatures {
    super::qemu::probe_emulator(arch, emulator)
        .map_or_else(GpuFeatures::default, |path| probe(&path, arch))
}

fn probe(path: &Path, arch: &str) -> GpuFeatures {
    type CacheKey = (PathBuf, SystemTime, &'static str);
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, GpuFeatures>>> = OnceLock::new();
    let Some(key) = path.canonicalize().ok().and_then(|path| {
        Some((path.clone(), path.metadata().ok()?.modified().ok()?, device(arch)))
    }) else {
        return GpuFeatures::default();
    };
    let cache = CACHE.get_or_init(Default::default);
    if let Some(features) = cache.lock().ok().and_then(|c| c.get(&key).copied()) {
        return features;
    }
    let Ok(help) = crate::files::path_text(path)
        .and_then(|path| super::run(path, &["-device", &format!("{},help", device(arch))]))
    else {
        return GpuFeatures::default();
    };
    let features = parse(&help);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, features);
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_properties_from_device_help() {
        let help = "virtio-vga-gl options:\n  blob=<bool>            - on/off (default: off)\n  \
                    venus=<bool>           - on/off (default: off)\n  hostmem=<size>\n";
        assert_eq!(
            parse(help),
            GpuFeatures {
                venus: true,
                neptune: false
            }
        );
        let help = format!("{help}  neptune=<bool>         - on/off (default: off)\n");
        assert_eq!(
            parse(&help),
            GpuFeatures {
                venus: true,
                neptune: true
            }
        );
        assert_eq!(parse("  context_init=<bool>\n"), GpuFeatures::default());
    }
}
