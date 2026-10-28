//! Turning a version document into a concrete list of things to download.

pub mod install;

pub use install::{build, InstallPlan, JavaRequirement};
