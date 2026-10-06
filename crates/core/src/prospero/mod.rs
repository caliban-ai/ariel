//! Client for prosperod's public HTTP and SSE API.
//!
//! All fleet control and fleet events reach Ariel through this module
//! (ADR 0002). [`ProsperoClient`] maps prospero's routes one to one;
//! [`FleetWatcher`] holds prospero's fleet-wide event stream open and forwards
//! every agent's events from it.

pub mod client;
pub mod sse;
pub mod types;
pub mod watcher;

pub use client::{ClientError, FleetFrom, ProsperoClient};
pub use watcher::{FleetWatcher, WatchConfig};
