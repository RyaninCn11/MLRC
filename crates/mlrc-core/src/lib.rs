//! Engine for MLRC, the Minecraft Launcher of Rust Command-line.
//!
//! Rules this crate lives by:
//!
//! - it never prints and never reads stdin: the CLI and the future GUI render,
//!   this crate only returns values and emits [`events::Event`]s;
//! - it does not depend on `clap`, `indicatif` or any terminal crate;
//! - every user-visible failure is a [`error::MlrcError`] carrying a stable code.

#![forbid(unsafe_code)]

pub mod auth;
pub mod doctor;
pub mod error;
pub mod events;
pub mod fetch;
pub mod install;
pub mod java;
pub mod launch;
pub mod meta;
pub mod plan;
pub mod port;
pub mod store;

pub use error::{ErrorCode, MlrcError, MlrcResult};
