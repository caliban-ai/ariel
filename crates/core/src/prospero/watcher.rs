//! Fleet-wide event feed, built from prospero's per-agent streams.
//!
//! Prospero has no fleet-wide event stream, only one per agent. The watcher
//! polls `GET /api/fleet` to discover agents, holds one stream open per agent,
//! and merges their events into a single channel. Each agent's stream is
//! delivered in order and exactly once, across gaps and reconnects.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::client::{ClientError, ProsperoClient};
use super::sse::StreamItem;
use super::types::{FleetEvent, WorkspaceHealth};

/// Timing for a [`FleetWatcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchConfig {
    /// How often to poll the fleet for new agents.
    pub poll_interval: Duration,
    /// Wait before reconnecting a dropped agent stream.
    pub reconnect_delay: Duration,
    /// After an agent reaches a terminal status, how long to keep listening for
    /// its final events. Prospero only closes a stream itself after
    /// `agent_finished`, so a killed or crashed agent's stream would otherwise
    /// stay open forever.
    pub linger: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            reconnect_delay: Duration::from_secs(1),
            linger: Duration::from_secs(10),
        }
    }
}

/// Polls the fleet and fans in every agent's events.
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
    /// stops the watcher and every agent stream.
    ///
    /// Agents already terminal on the first poll are history and are not
    /// replayed. Every agent seen after that, including one that finished
    /// between two polls, has its stream delivered from the start.
    pub fn spawn(self, buffer: usize) -> (mpsc::Receiver<FleetEvent>, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(buffer);
        (rx, tokio::spawn(run(self.client, self.config, tx)))
    }
}

async fn run(client: ProsperoClient, config: WatchConfig, tx: mpsc::Sender<FleetEvent>) {
    // Agent id -> (workspace, follower task).
    let mut followers: HashMap<String, (String, JoinHandle<()>)> = HashMap::new();
    // Every agent ever followed or deliberately skipped, so none is followed
    // twice: a second follower would redeliver the agent's history.
    let mut seen: HashSet<String> = HashSet::new();
    let mut first_poll = true;

    let mut ticker = tokio::time::interval(config.poll_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            () = tx.closed() => break,
        }
        let fleet = match client.fleet().await {
            Ok(fleet) => fleet,
            Err(error) => {
                tracing::warn!(%error, "prospero fleet poll failed");
                continue;
            }
        };

        // An agent missing from a healthy workspace is gone, and its stream
        // would only ever send keepalives. An unreachable workspace lists no
        // agents, so absence there proves nothing.
        let healthy: HashSet<&str> = fleet
            .workspaces
            .iter()
            .filter(|w| w.health == WorkspaceHealth::Healthy)
            .map(|w| w.name.as_str())
            .collect();
        let listed: HashSet<&str> = fleet.agents().map(|a| a.id.as_str()).collect();
        followers.retain(|id, (workspace, follower)| {
            let vanished = healthy.contains(workspace.as_str()) && !listed.contains(id.as_str());
            if vanished {
                follower.abort();
            }
            !vanished && !follower.is_finished()
        });

        for agent in fleet.agents() {
            if !seen.insert(agent.id.clone()) || (first_poll && agent.status.is_terminal()) {
                continue;
            }
            let follower =
                tokio::spawn(follow(client.clone(), agent.id.clone(), config, tx.clone()));
            followers.insert(agent.id.clone(), (agent.workspace.clone(), follower));
        }
        first_poll = false;
    }

    for (_, follower) in followers.values() {
        follower.abort();
    }
}

/// Deliver one agent's stream, reconnecting until it ends.
async fn follow(
    client: ProsperoClient,
    agent_id: String,
    config: WatchConfig,
    tx: mpsc::Sender<FleetEvent>,
) {
    // The next seq to deliver. `from` is inclusive, so it is also where a
    // reconnect resumes.
    let mut next = 0;
    loop {
        match client.stream(&agent_id, next).await {
            Ok(stream) => {
                let mut stream = std::pin::pin!(stream);
                let mut hang_up_at: Option<Instant> = None;
                loop {
                    let item = match hang_up_at {
                        None => stream.next().await,
                        Some(deadline) => {
                            match tokio::time::timeout_at(deadline, stream.next()).await {
                                Ok(item) => item,
                                Err(_) => return,
                            }
                        }
                    };
                    // prosperod closed the stream: reconnect and resume.
                    let Some(item) = item else { break };
                    match item {
                        Ok(StreamItem::Event(event)) => {
                            // A replay after a reconnect can overlap what was
                            // already delivered.
                            if event.seq < next {
                                continue;
                            }
                            next = event.seq + 1;
                            if event.kind.is_terminal_status() && hang_up_at.is_none() {
                                hang_up_at = Some(Instant::now() + config.linger);
                            }
                            let ends = event.kind.ends_stream();
                            if tx.send(event).await.is_err() || ends {
                                return;
                            }
                        }
                        // The server replays the skipped events on this same
                        // stream, so the cursor stays where it is.
                        Ok(StreamItem::Gap(gap)) => tracing::debug!(
                            agent_id = %agent_id,
                            skipped = gap.skipped,
                            last_seq = gap.last_seq,
                            "prospero stream gap; the server replays the skipped events"
                        ),
                        Err(ClientError::Json(error)) => tracing::warn!(
                            agent_id = %agent_id,
                            %error,
                            "skipping undecodable prospero frame"
                        ),
                        Err(error) => {
                            tracing::warn!(agent_id = %agent_id, %error, "prospero stream dropped");
                            break;
                        }
                    }
                }
                if hang_up_at.is_some() {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(agent_id = %agent_id, %error, "prospero stream connect failed");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(config.reconnect_delay) => {}
            () = tx.closed() => return,
        }
    }
}
