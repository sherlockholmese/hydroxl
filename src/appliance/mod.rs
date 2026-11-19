mod archive;
mod export;
mod import;
mod manifest;
mod ovf;
mod vmdk;
pub use export::export_ova;
pub use import::import;
use std::path::PathBuf;
#[derive(Clone, Debug)]
pub struct ImportSpec {
    pub path: PathBuf,
    pub location: PathBuf,
    pub name: String,
    pub architecture: String,
    pub qemu_build: crate::backend::QemuBuild,
    pub execution: String,
    pub firmware: String,
}
pub(super) struct Definition {
    pub(crate) name: String,
    pub(crate) cpus: u32,
    pub(crate) memory_mib: u64,
    pub(crate) disks: Vec<PathBuf>,
    pub(crate) networks: usize,
    pub(crate) uefi: bool,
    pub(crate) architecture: String,
}
