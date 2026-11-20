//! In-app guest display.
pub mod vnc;

mod damage;
mod dbus;
pub(crate) mod frame_stats;
pub mod geometry;
mod graphics_fd;
pub mod input;
mod keys;
pub(crate) mod scanout;
pub(crate) mod session;
mod spice;
#[cfg(test)]
mod texture_tests;
pub(crate) mod thumbnail;
