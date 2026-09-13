//! Client for prosperod's public HTTP and SSE API.
//!
//! All fleet control and fleet events reach Ariel through this module
//! (ADR 0002). [`ProsperoClient`] maps prospero's routes one to one;
//! [`FleetWatcher`] builds the fleet-wide event feed prospero does not offer,
//! by polling the fleet and fanning in one stream per agent.

pub mod client;
pub mod sse;
pub mod types;
pub mod watcher;

pub use client::{ClientError, ProsperoClient};
pub use watcher::{FleetWatcher, WatchConfig};
