//! HTTP: client, download queue, mirrors, checksums, and proxy detection for child JVMs.

pub mod checksum;
pub mod client;
pub mod download;
pub mod mirror;
pub mod proxy;

pub use checksum::Checksum;
pub use client::HttpClient;
pub use mirror::Mirror;
