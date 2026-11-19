//! What the installed QEMU's 3D VirtIO GPU can do. `venus` (Vulkan) is in upstream
//! QEMU; `neptune` (Direct3D 11/12, needed by the Windows viogpu3d driver) comes from
//! UTM's QEMU and virglrenderer forks. Each property only exists when QEMU was built
//! against a virglrenderer that supports it.
use std::{
    collections::HashMap,
    process::Command,
    sync::{Mutex, OnceLock},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuFeatures {
    pub venus: bool,
    pub neptune: bool,
}

/// Size of the window guest blob resources are mapped through, as UTM sets it.
pub const HOSTMEM_BYTES: u64 = 8 << 30;

fn device(arch: &str) -> &'static str {
    if matches!(arch, "x86_64" | "i686") {
        "virtio-vga-gl"
    } else {
        "virtio-gpu-gl-pci"
    }
}

pub(crate) fn parse(help: &str) -> GpuFeatures {
    let has = |name: &str| help.lines().any(|line| line.trim_start().starts_with(&format!("{name}=")));
    GpuFeatures {
        venus: has("venus"),
        neptune: has("neptune"),
    }
}

/// Probed once per architecture and cached for the session.
pub fn gpu_features(arch: &str) -> GpuFeatures {
    static CACHE: OnceLock<Mutex<HashMap<String, GpuFeatures>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(features) = cache.lock().ok().and_then(|c| c.get(arch).copied()) {
        return features;
    }
    let emulator = format!("qemu-system-{arch}");
    let features = Command::new(emulator)
        .args(["-device", &format!("{},help", device(arch))])
        .output()
        .ok()
        .map(|out| parse(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default();
    if let Ok(mut cache) = cache.lock() {
        cache.insert(arch.to_owned(), features);
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
        assert_eq!(parse(help), GpuFeatures { venus: true, neptune: false });
        let help = format!("{help}  neptune=<bool>         - on/off (default: off)\n");
        assert_eq!(parse(&help), GpuFeatures { venus: true, neptune: true });
        assert_eq!(parse("  context_init=<bool>\n"), GpuFeatures::default());
    }
}
