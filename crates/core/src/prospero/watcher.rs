//! The fleet-wide event feed.
//!
//! prosperod serves one SSE connection carrying every agent's events
//! (prospero#219), so the watcher holds that connection open and forwards what
//! arrives. Every event carries a **fleet cursor** — durable insertion order
//! across all streams — and a dropped connection resumes strictly after the
//! last cursor seen, so a reconnect neither skips an event nor repeats one.
//!
//! Before prospero v0.9 there was no such endpoint, and this was built by
//! polling `GET /api/fleet` to discover agents and holding one stream per
//! agent. That missed any agent which started and finished between two polls
//! (#55). `GET /api/fleet` is still how `/ariel status` reads a snapshot; it is
//! no longer how Ariel learns that something happened.

use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::client::{ClientError, FleetFrom, ProsperoClient};
use super::sse::CursoredEvent;
use super::types::FleetEvent;

/// Timing for a [`FleetWatcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchConfig {
    /// Wait before reconnecting a dropped fleet stream.
    pub reconnect_delay: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            reconnect_delay: Duration::from_secs(1),
        }
    }
}

/// Holds prosperod's fleet stream open and forwards every agent's events.
#[derive(Debug, Clone)]
pub struct FleetWatcher {
    client: ProsperoClient,
    config: WatchConfig,
}

impl FleetWatcher {
    pub fn new(client: ProsperoClient, config: WatchConfig) -> Self {
        Self { client, config }
    }

    /// Start watching. Events arrive on the returned receiver; dropping it
    /// stops the watcher and closes the connection.
    ///
    /// The first connection asks for `from=now`, so nothing that happened
    /// before this moment is announced
    /// ([ADR 0011](../../docs/adr/0011-no-replay-after-a-restart.md)). The
    /// cursor it follows after that lives in memory only — Ariel keeps no state
    /// of its own ([ADR 0003](../../docs/adr/0003-no-state-of-its-own.md)) — so
    /// a restart starts from `now` again rather than resuming.
    pub fn spawn(self, buffer: usize) -> (mpsc::Receiver<FleetEvent>, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(buffer);
        (rx, tokio::spawn(run(self.client, self.config, tx)))
    }
}

async fn run(client: ProsperoClient, config: WatchConfig, tx: mpsc::Sender<FleetEvent>) {
    let mut from = FleetFrom::Now;
    loop {
        match client.fleet_stream(from).await {
            Ok(stream) => {
                let mut stream = std::pin::pin!(stream);
                loop {
                    let item = tokio::select! {
                        item = stream.next() => item,
                        () = tx.closed() => return,
                    };
                    // prosperod closed the stream: reconnect and resume.
                    let Some(item) = item else { break };
                    match item {
                        Ok(CursoredEvent { cursor, event }) => {
                            // Advance only on a cursor that can be resumed
                            // from. An event that somehow arrived without one
                            // is still delivered; the next reconnect simply
                            // resumes from the last cursor that had one, which
                            // costs a repeat rather than a loss.
                            if let Some(cursor) = cursor {
                                from = FleetFrom::After(cursor);
                            }
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                        Err(ClientError::Json(error)) => {
                            tracing::warn!(%error, "skipping undecodable prospero frame");
                        }
                        Err(error) => {
                            tracing::warn!(%error, "prospero fleet stream dropped");
                            break;
                        }
                    }
                }
            }
            // Retrying cannot fix a refused credential, so say so plainly
            // rather than as one more transient failure.
            Err(error @ ClientError::Auth { .. }) => {
                tracing::error!(
                    %error,
                    "prosperod refused Ariel's token; set ARIEL_PROSPERO_TOKEN_FILE to a token \
                     with at least `read` scope"
                );
            }
            Err(error) => {
                tracing::warn!(%error, "prospero fleet stream connect failed");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(config.reconnect_delay) => {}
            () = tx.closed() => return,
        }
    }
}
