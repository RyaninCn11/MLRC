//! HTTP: client, download queue, mirrors, and proxy detection for child JVMs.

pub mod client;
pub mod download;
pub mod mirror;
pub mod proxy;

pub use client::HttpClient;
pub use mirror::Mirror;
