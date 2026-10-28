//! Identities. Microsoft accounts arrive in M5; offline identities are the only
//! kind for now, and they are also what a GUI needs to start a local world.

pub mod offline;

pub use offline::{offline, uuid_without_dashes, Identity};
