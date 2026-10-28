//! Managed Java runtimes.
//!
//! Hard constraint C6: unless the user opts in with `customJava`, the host JDK is
//! never consulted. A machine has one `JAVA_HOME` but Minecraft spans Java 8, 17,
//! 21 and 25, so borrowing the host guarantees that some version breaks eventually.

pub mod discover;
pub mod runtime;

pub use discover::{JavaInstallation, JavaSource};
