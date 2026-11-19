use std::{env, ffi::OsStr, path::PathBuf, process::Command};

fn main() {
    for input in [
        "scripts/build-neptune.sh",
        "scripts/neptune-sources.txt",
        "scripts/triton",
        "patches/neptune/qemu-11.1.3.patch",
    ] {
        println!("cargo:rerun-if-changed={input}");
    }
    for name in ["HYDROXL_QEMU_BUILD_PREFIX", "HYDROXL_QEMU_DESTDIR", "HYDROXL_QEMU_ARCHIVE_CACHE"]
    {
        println!("cargo:rerun-if-env-changed={name}");
    }

    // Versions shown in About come from the pinned sources, so they cannot drift.
    let sources = std::fs::read_to_string("scripts/neptune-sources.txt")
        .expect("scripts/neptune-sources.txt");
    let urls = || sources.split_whitespace().filter(|word| word.starts_with("https://"));
    let qemu = urls()
        .filter_map(|url| url.rsplit('/').next()?.strip_prefix("qemu-")?.strip_suffix(".tar.xz"))
        .next()
        .unwrap_or_default();
    let triton = urls()
        .filter_map(|url| url.split_once("/releases/download/")?.1.split('/').next())
        .next()
        .unwrap_or_default();
    println!("cargo:rustc-env=HYDROXL_QEMU_VERSION={qemu}");
    println!("cargo:rustc-env=HYDROXL_TRITON_VERSION={triton}");

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let profile = out
        .ancestors()
        .find(|path| path.file_name() == Some(OsStr::new("build")))
        .and_then(|path| path.parent())
        .expect("Cargo build profile directory");
    // Share the debug build's native bundle across Cargo profiles for this target.
    let build_parent = profile.with_file_name("debug");
    let work = build_parent.join("hydroxl-neptune-build");
    let prefix = env::var_os("HYDROXL_QEMU_BUILD_PREFIX")
        .map_or_else(|| build_parent.join("hydroxl-neptune"), PathBuf::from);
    let destdir = env::var_os("HYDROXL_QEMU_DESTDIR").unwrap_or_default();
    let cache = env::var_os("HYDROXL_QEMU_ARCHIVE_CACHE")
        .map_or_else(|| work.join("archives"), PathBuf::from);

    let status = Command::new("bash")
        .args(["scripts/build-neptune.sh"])
        .arg(&prefix)
        .arg(work)
        .arg(destdir)
        .arg(cache)
        .env(
            "HYDROXL_BUILD_JOBS",
            env::var("HYDROXL_BUILD_JOBS")
                .or_else(|_| env::var("NUM_JOBS"))
                .unwrap_or_else(|_| "1".into()),
        )
        .status()
        .expect("Could not launch the Neptune build script");
    assert!(status.success(), "Neptune QEMU build failed. See docs/neptune.md for dependencies.");
    println!("cargo:rustc-env=HYDROXL_BUILT_QEMU_ROOT={}", prefix.display());
}
