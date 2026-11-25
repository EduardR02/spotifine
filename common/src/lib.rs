//! Shared wire types and file replacement for the Tauri shell and playback engine.
//!
//! The engine speaks line-delimited JSON over stdin/stdout. Both processes use
//! these types to agree on the wire format without linking each other's code.

pub mod atomic;
pub mod protocol;
