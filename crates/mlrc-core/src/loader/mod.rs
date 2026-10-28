//! Loader support.
//!
//! A loader is not a separate install format: it publishes a version document that
//! inherits from a vanilla version, and the existing pipeline does the rest.

pub mod fabric;
pub mod forge;

pub use fabric::FabricProfile;
