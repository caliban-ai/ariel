//! Typed HTTP client for prosperod's routes.

use std::collections::VecDeque;
use std::time::Duration;

use futures_util::{Stream, StreamExt, stream};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde::de::DeserializeOwned;

use super::sse::{FrameDecoder, StreamItem};
use super::types::{
    ApiErrorBody, FleetSnapshot, InputRequest, RespawnedResponse, SessionInfo, SpawnRequest,
    SpawnedResponse,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Applies to every request except the event stream, which stays open.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a prospero call failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("invalid prospero base URL `{url}`: {reason}")]
    BaseUrl { url: String, reason: String },
    #[error("request to prospero failed: {0}")]
    Transport(#[from] reqwest::Error),
    /// prosperod refused the request's credentials: `401` for a missing or
    /// unknown token, `403` for a token whose scope is too low (prospero
    /// ADR 0010). Retrying will not help until the token is fixed.
    #[error("prosperod refused Ariel's credentials ({status}): {message}")]
    Auth { status: StatusCode, message: String },
    /// prosperod answered with a non-success status. `kind` is prospero's
    /// error kind (`not_found`, `invalid_state`, ...) when the body carried one.
    #[error("prospero returned {status}: {message}")]
    Api {
        status: StatusCode,
        kind: Option<String>,
        message: String,
    },
    #[error("invalid JSON exchanged with prospero: {0}")]
    Json(#[from] serde_json::Error),
}

/// A client for one prosperod.
///
/// Speaks plain HTTP; TLS arrives with the deployment decision (#5). With a
/// token (#46), every request carries it as `Authorization: Bearer`, including
/// the event stream.
#[derive(Debug, Clone)]
pub struct ProsperoClient {
    base: Url,
    http: Client,
    token: Option<BearerToken>,
}

/// A prosperod API token. Formats as `[redacted]`, so it cannot reach logs.
#[derive(Clone)]
struct BearerToken(String);

impl std::fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl ProsperoClient {
    /// A client for the prosperod at `base`, e.g. `http://127.0.0.1:7878`. A
    /// path prefix, as behind a reverse proxy, is kept.
    pub fn new(base: &str) -> Result<Self, ClientError> {
        let invalid = |reason: String| ClientError::BaseUrl {
            url: base.to_owned(),
            reason,
        };
        let url = Url::parse(base).map_err(|e| invalid(e.to_string()))?;
        if url.cannot_be_a_base() {
            return Err(invalid("not a hierarchical URL".to_owned()));
        }
        let http = Client::builder().connect_timeout(CONNECT_TIMEOUT).build()?;
        Ok(Self {
            base: url,
            http,
            token: None,
        })
    }

    /// This client, sending `token` with every request.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(BearerToken(token.into()));
        self
    }

    /// `GET /api/session`: who prosperod takes this client to be, or that it
    /// runs with authentication off.
    pub async fn session(&self) -> Result<SessionInfo, ClientError> {
        json(
            self.send(self.http.get(self.endpoint(&["api", "session"])))
                .await?,
        )
        .await
    }

    /// `GET /api/fleet`.
    pub async fn fleet(&self) -> Result<FleetSnapshot, ClientError> {
        json(
            self.send(self.http.get(self.endpoint(&["api", "fleet"])))
                .await?,
        )
        .await
    }

    /// `POST /api/workspaces/{workspace}/agents`.
    pub async fn spawn(
        &self,
        workspace: &str,
        request: &SpawnRequest,
    ) -> Result<SpawnedResponse, ClientError> {
        let url = self.endpoint(&["api", "workspaces", workspace, "agents"]);
        let body = serde_json::to_vec(request)?;
        let response = self.send(with_json(self.http.post(url), body)).await?;
        json(response).await
    }

    /// `POST /api/agents/{id}/kill`.
    pub async fn kill(&self, agent_id: &str) -> Result<(), ClientError> {
        self.post_empty(&["api", "agents", agent_id, "kill"]).await
    }

    /// `POST /api/agents/{id}/respawn`. The respawned agent has a new id.
    pub async fn respawn(&self, agent_id: &str) -> Result<RespawnedResponse, ClientError> {
        let url = self.endpoint(&["api", "agents", agent_id, "respawn"]);
        json(self.send(self.http.post(url)).await?).await
    }

    /// `POST /api/agents/{id}/input`.
    pub async fn input(&self, agent_id: &str, text: &str) -> Result<(), ClientError> {
        let url = self.endpoint(&["api", "agents", agent_id, "input"]);
        let body = serde_json::to_vec(&InputRequest {
            text: text.to_owned(),
        })?;
        self.send(with_json(self.http.post(url), body)).await?;
        Ok(())
    }

    /// `POST /api/agents/{id}/end-input`.
    pub async fn end_input(&self, agent_id: &str) -> Result<(), ClientError> {
        self.post_empty(&["api", "agents", agent_id, "end-input"])
            .await
    }

    /// `GET /api/agents/{id}/stream?from={from}`: the agent's events with
    /// `seq >= from` replayed from history, then tailed live.
    ///
    /// The stream ends when prosperod closes it, which it does after
    /// `agent_finished`. An undecodable frame yields [`ClientError::Json`] and
    /// the stream continues; a transport failure ends it.
    pub async fn stream(
        &self,
        agent_id: &str,
        from: u64,
    ) -> Result<impl Stream<Item = Result<StreamItem, ClientError>> + Send + 'static, ClientError>
    {
        let mut url = self.endpoint(&["api", "agents", agent_id, "stream"]);
        url.query_pairs_mut().append_pair("from", &from.to_string());
        let response = check(
            self.authorize(self.http.get(url))
                .header(ACCEPT, "text/event-stream")
                .send()
                .await?,
        )
        .await?;

        let state = (
            Box::pin(response.bytes_stream()),
            FrameDecoder::new(),
            VecDeque::new(),
        );
        Ok(stream::unfold(
            state,
            |(mut bytes, mut decoder, mut ready)| async move {
                loop {
                    if let Some(item) = ready.pop_front() {
                        return Some((item, (bytes, decoder, ready)));
                    }
                    match bytes.next().await? {
                        Ok(chunk) => {
                            for frame in decoder.push(&chunk) {
                                match StreamItem::from_frame(&frame) {
                                    Ok(Some(item)) => ready.push_back(Ok(item)),
                                    Ok(None) => {}
                                    Err(error) => ready.push_back(Err(ClientError::Json(error))),
                                }
                            }
                        }
                        Err(error) => {
                            return Some((
                                Err(ClientError::Transport(error)),
                                (bytes, decoder, ready),
                            ));
                        }
                    }
                }
            },
        ))
    }

    async fn post_empty(&self, segments: &[&str]) -> Result<(), ClientError> {
        self.send(self.http.post(self.endpoint(segments))).await?;
        Ok(())
    }

    async fn send(&self, request: RequestBuilder) -> Result<Response, ClientError> {
        check(
            self.authorize(request)
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await?,
        )
        .await
    }

    /// `request` with the bearer token, when this client has one.
    fn authorize(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.token {
            Some(BearerToken(token)) => request.header(AUTHORIZATION, format!("Bearer {token}")),
            None => request,
        }
    }

    /// The base URL extended by percent-encoded path segments.
    fn endpoint(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("base URL is hierarchical, checked in new")
            .pop_if_empty()
            .extend(segments);
        url
    }
}

fn with_json(request: RequestBuilder, body: Vec<u8>) -> RequestBuilder {
    request.header(CONTENT_TYPE, "application/json").body(body)
}

/// Turn a non-success status into [`ClientError::Api`], reading prospero's
/// `{"error", "kind"}` body when there is one.
async fn check(response: Response) -> Result<Response, ClientError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let text = response.text().await.unwrap_or_default();
    let (kind, message) = match serde_json::from_str::<ApiErrorBody>(&text) {
        Ok(body) => (Some(body.kind), body.error),
        Err(_) => (None, text),
    };
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(ClientError::Auth { status, message });
    }
    Err(ClientError::Api {
        status,
        kind,
        message,
    })
}

async fn json<T: DeserializeOwned>(response: Response) -> Result<T, ClientError> {
    Ok(serde_json::from_slice(&response.bytes().await?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_percent_encode_segments() {
        let client = ProsperoClient::new("http://127.0.0.1:7878").unwrap();
        assert_eq!(
            client
                .endpoint(&["api", "agents", "a b/c", "kill"])
                .as_str(),
            "http://127.0.0.1:7878/api/agents/a%20b%2Fc/kill"
        );
    }

    #[test]
    fn endpoints_keep_a_base_path_prefix() {
        for base in ["http://host/prospero", "http://host/prospero/"] {
            let client = ProsperoClient::new(base).unwrap();
            assert_eq!(
                client.endpoint(&["api", "fleet"]).as_str(),
                "http://host/prospero/api/fleet"
            );
        }
    }

    #[test]
    fn rejects_unusable_base_urls() {
        for base in ["not a url", "mailto:ops@example.com"] {
            assert!(matches!(
                ProsperoClient::new(base),
                Err(ClientError::BaseUrl { .. })
            ));
        }
    }
}
