//! Mirrored prospero wire types.
//!
//! Ariel takes no dependency on prospero crates (ADR 0002), so the subset of
//! prospero's HTTP and SSE contract Ariel reads is mirrored here and pinned by
//! golden fixtures in `tests/fixtures/prospero/` (ADR 0005). Fields Ariel does not use are
//! left out; serde ignores them on decode.
//!
//! Every enum Ariel matches on carries an `Unknown` fallback, so a newer
//! prosperod that adds a status, stream, health state or event kind does not
//! break decoding.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `GET /api/fleet`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetSnapshot {
    pub host: String,
    pub workspaces: Vec<Workspace>,
}

impl FleetSnapshot {
    /// Every agent across every workspace.
    pub fn agents(&self) -> impl Iterator<Item = &Agent> {
        self.workspaces.iter().flat_map(|w| w.agents.iter())
    }
}

/// One supervised workspace and its agents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    pub health: WorkspaceHealth,
    pub agents: Vec<Agent>,
}

/// One agent as listed in a fleet snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Opaque caliban agent id.
    pub id: String,
    pub name: String,
    pub workspace: String,
    pub status: AgentStatus,
    /// RFC 3339.
    pub started_at: String,
    pub isolated: bool,
    pub interactive: bool,
}

/// Agent lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Spawning,
    Running,
    Idle,
    Killed,
    Done,
    Failed,
    Crashed,
    /// A status this build of Ariel does not know.
    #[serde(other)]
    Unknown,
}

impl AgentStatus {
    /// The agent will produce no further events. An unknown status is not
    /// terminal: Ariel keeps listening rather than dropping a live agent.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AgentStatus::Killed | AgentStatus::Done | AgentStatus::Failed | AgentStatus::Crashed
        )
    }
}

/// Workspace reachability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkspaceHealth {
    Healthy,
    Unreachable {
        reason: String,
    },
    /// A health state this build of Ariel does not know.
    #[serde(other)]
    Unknown,
}

/// One event on an agent's stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetEvent {
    /// Monotonic within one agent's stream.
    pub seq: u64,
    /// RFC 3339.
    pub ts: String,
    /// Empty for fleet-level events.
    pub repo: String,
    /// Empty for workspace-level events.
    pub agent_id: String,
    pub kind: EventKind,
}

/// What happened. Tagged on the wire by an inner `kind` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    AgentSpawned,
    AgentDiscovered,
    AgentInit {
        model: String,
        tools: Vec<String>,
        session_id: String,
    },
    StatusChanged {
        from: AgentStatus,
        to: AgentStatus,
    },
    Output {
        stream: OutputStream,
        chunk: String,
    },
    ToolStarted {
        #[serde(default)]
        id: String,
        name: String,
        input: Value,
    },
    ToolFinished {
        #[serde(default)]
        id: String,
        name: String,
        ok: bool,
    },
    AgentFinished {
        /// Free text, e.g. `success` or `max_turns`.
        outcome: String,
        cost_usd: f64,
        turns: u32,
    },
    AgentGone,
    StorePersistFailed {
        lost_seq: u64,
        detail: String,
    },
    RepoHealth {
        state: WorkspaceHealth,
    },
    /// An event kind this build of Ariel does not know.
    #[serde(other)]
    Unknown,
}

impl EventKind {
    /// The server closes the stream after this event.
    pub fn ends_stream(&self) -> bool {
        matches!(self, EventKind::AgentFinished { .. })
    }

    /// The agent reached a terminal status, after which the server keeps the
    /// stream open but sends nothing more.
    pub fn is_terminal_status(&self) -> bool {
        matches!(self, EventKind::StatusChanged { to, .. } if to.is_terminal())
    }
}

/// Which agent output channel a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Thinking,
    #[serde(other)]
    Unknown,
}

/// Payload of an SSE `event: gap` frame: the subscriber fell behind and the
/// server skipped `skipped` events. The server replays them from
/// `last_seq + 1` on the same stream, so the client's cursor does not move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapSignal {
    pub skipped: u64,
    pub last_seq: u64,
}

/// `POST /api/workspaces/{workspace}/agents` body.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `worktree` (prospero's default) or `shared`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(default)]
    pub interactive: bool,
}

impl SpawnRequest {
    /// A spawn with only a prompt; everything else takes prospero's default.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            ..Self::default()
        }
    }
}

/// `POST /api/workspaces/{workspace}/agents` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnedResponse {
    pub agent_id: String,
    pub workspace: String,
    pub isolated: bool,
    /// `false` when prospero attached to an identical existing run. Absent
    /// from older daemons, which always created.
    #[serde(default = "created_by_default")]
    pub created: bool,
}

fn created_by_default() -> bool {
    true
}

/// `POST /api/agents/{id}/respawn` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RespawnedResponse {
    pub agent_id: String,
}

/// `POST /api/agents/{id}/input` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRequest {
    pub text: String,
}

/// Error body prospero returns with every non-2xx status it produces itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub error: String,
    pub kind: String,
}
