//! In-app guest display.
pub mod vnc;

mod damage;
mod dbus;
pub mod geometry;
mod graphics_fd;
pub mod input;
mod keys;
mod scanout;
pub(crate) mod session;
mod spice;
#[cfg(test)]
mod texture_tests;
pub(crate) mod thumbnail;
