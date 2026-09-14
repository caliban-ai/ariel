//! How `DiscordProvider` answers interactions and maps Discord's responses,
//! checked against a stub of Discord's REST API that records every request.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ariel_core::chat::{
    ChannelRef, ChatProvider, Command, Destination, Inbound, Message, ProviderError, Visibility,
};
use ariel_discord::{DiscordConfig, DiscordProvider};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde_json::{Value, json};

const GUILD: &str = "100";
const CHANNEL: &str = "200";
const USER: &str = "300";
const APP: &str = "400";

/// One request the stub received.
#[derive(Debug, Clone)]
struct Request {
    method: String,
    path: String,
    body: Value,
}

#[derive(Default)]
struct Stub {
    requests: Mutex<Vec<Request>>,
    ids: AtomicU64,
}

impl Stub {
    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    fn message(&self) -> Value {
        json!({
            "id": (9_000 + self.ids.fetch_add(1, Ordering::SeqCst)).to_string(),
            "channel_id": CHANNEL, "content": "", "embeds": [], "attachments": [],
            "components": [], "mentions": [], "mention_roles": [],
            "mention_everyone": false, "pinned": false, "tts": false, "type": 0,
            "timestamp": "2026-09-13T00:00:00+00:00",
            "author": { "id": APP, "username": "ariel", "discriminator": "0", "avatar": null }
        })
    }
}

async fn record(State(stub): State<Arc<Stub>>, method: Method, uri: Uri, body: Bytes) -> Response {
    let path = uri.path().to_owned();
    stub.requests.lock().unwrap().push(Request {
        method: method.to_string(),
        path: path.clone(),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    });

    let discord_error = |status: StatusCode, code: u64| {
        (
            status,
            axum::Json(json!({ "code": code, "message": "stub error" })),
        )
            .into_response()
    };
    if path.contains("/channels/403/") {
        return discord_error(StatusCode::FORBIDDEN, 50013);
    }
    if path.contains("/channels/404/") {
        return discord_error(StatusCode::NOT_FOUND, 10003);
    }
    if path.ends_with("/callback") {
        return StatusCode::NO_CONTENT.into_response();
    }
    axum::Json(stub.message()).into_response()
}

async fn stub() -> (Arc<Stub>, Arc<DiscordProvider>) {
    let stub = Arc::new(Stub::default());
    let app = Router::new().fallback(record).with_state(Arc::clone(&stub));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let provider = DiscordProvider::new(DiscordConfig {
        token: "test-token".to_owned(),
        guild_id: GUILD.parse().unwrap(),
        application_id: APP.parse().unwrap(),
        api_proxy: Some(addr.to_string()),
    })
    .expect("build provider");
    (stub, Arc::new(provider))
}

/// A slash command interaction for `/<command> <sub>` with string options.
fn interaction(id: u64, command: &str, sub: &str, options: &[(&str, &str)]) -> Value {
    let options: Vec<Value> = options
        .iter()
        .map(|(name, value)| json!({ "name": name, "type": 3, "value": value }))
        .collect();
    json!({
        "id": id.to_string(), "application_id": APP, "type": 2, "token": format!("tok-{id}"),
        "guild_id": GUILD, "channel": { "id": CHANNEL, "type": 0 },
        "authorizing_integration_owners": {}, "entitlements": [],
        "member": { "user": { "id": USER, "username": "someone", "discriminator": "0", "avatar": null },
                    "roles": [], "joined_at": "2026-01-01T00:00:00+00:00", "deaf": false, "mute": false, "flags": 0 },
        "data": { "id": "600", "name": command, "type": 1,
                  "options": [{ "name": sub, "type": 1, "options": options }] }
    })
}

fn deliver(provider: &DiscordProvider, payload: Value) {
    provider.deliver_interaction(serde_json::from_value(payload).expect("valid interaction"));
}

async fn next_command(inbound: &mut BoxStream<'static, Inbound>) -> Command {
    match tokio::time::timeout(Duration::from_secs(2), inbound.next()).await {
        Ok(Some(Inbound::Command(command))) => command,
        _ => panic!("expected an inbound command"),
    }
}

fn callbacks(stub: &Stub) -> Vec<Request> {
    stub.requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/callback"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn string_options_become_args() {
    let (_stub, provider) = stub().await;
    let mut inbound = provider.inbound();

    deliver(
        &provider,
        interaction(
            1,
            "ariel",
            "spawn",
            &[("workspace", "caliban"), ("prompt", "fix the tests")],
        ),
    );
    let command = next_command(&mut inbound).await;

    assert_eq!(command.name, "spawn");
    assert_eq!(command.args.get("workspace"), Some("caliban"));
    assert_eq!(command.args.get("prompt"), Some("fix the tests"));
    assert_eq!(command.user.user, USER);
    assert_eq!(
        command.at,
        Destination::Channel(ChannelRef::new("discord", GUILD, CHANNEL))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn interactions_for_other_commands_are_ignored() {
    let (_stub, provider) = stub().await;
    let mut inbound = provider.inbound();

    deliver(&provider, interaction(2, "someone-else", "status", &[]));

    let next = tokio::time::timeout(Duration::from_millis(300), inbound.next()).await;
    assert!(next.is_err(), "no command should arrive");
}

#[tokio::test(flavor = "multi_thread")]
async fn replying_straight_away_sends_the_initial_response() {
    let (stub, provider) = stub().await;
    let mut inbound = provider.inbound();
    deliver(&provider, interaction(3, "ariel", "status", &[]));
    let command = next_command(&mut inbound).await;

    command
        .responder
        .reply(&Message::text("ok"), Visibility::Public)
        .await
        .unwrap();

    let sent = callbacks(&stub);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].path, "/api/v10/interactions/3/tok-3/callback");
    assert_eq!(sent[0].body["type"], 4, "channel message with source");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_public_reply_after_deferring_edits_the_original_response() {
    let (stub, provider) = stub().await;
    let mut inbound = provider.inbound();
    deliver(&provider, interaction(4, "ariel", "spawn", &[]));
    let command = next_command(&mut inbound).await;

    command.responder.defer(Visibility::Public).await.unwrap();
    command.responder.defer(Visibility::Public).await.unwrap();
    let reply = command
        .responder
        .reply(&Message::text("spawned"), Visibility::Public)
        .await
        .unwrap();

    let sent = callbacks(&stub);
    assert_eq!(sent.len(), 1, "deferring twice sends one callback");
    assert_eq!(sent[0].body["type"], 5, "deferred channel message");
    let edits: Vec<_> = stub
        .requests()
        .into_iter()
        .filter(|r| {
            r.method == "PATCH" && r.path == "/api/v10/webhooks/400/tok-4/messages/@original"
        })
        .collect();
    assert_eq!(edits.len(), 1);
    assert!(
        reply.message.parse::<u64>().is_ok(),
        "ref is the edited message ID"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_reply_after_deferring_is_an_ephemeral_followup() {
    let (stub, provider) = stub().await;
    let mut inbound = provider.inbound();
    deliver(&provider, interaction(5, "ariel", "link", &[]));
    let command = next_command(&mut inbound).await;

    command.responder.defer(Visibility::Public).await.unwrap();
    command
        .responder
        .reply(&Message::text("linked"), Visibility::Private)
        .await
        .unwrap();

    let followups: Vec<_> = stub
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path == "/api/v10/webhooks/400/tok-5")
        .collect();
    assert_eq!(followups.len(), 1);
    assert_eq!(followups[0].body["flags"], 64, "ephemeral");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_backend_defers_when_the_core_is_slow() {
    let (stub, provider) = stub().await;
    let mut inbound = provider.inbound();
    deliver(&provider, interaction(6, "ariel", "spawn", &[]));
    let _command = next_command(&mut inbound).await;

    tokio::time::sleep(Duration::from_millis(2_500)).await;

    let sent = callbacks(&stub);
    assert_eq!(sent.len(), 1, "auto-defer sent exactly one callback");
    assert_eq!(sent[0].body["type"], 5, "deferred channel message");
}

#[tokio::test(flavor = "multi_thread")]
async fn discord_errors_map_to_provider_errors() {
    let (_stub, provider) = stub().await;
    let at = |channel: &str| Destination::Channel(ChannelRef::new("discord", GUILD, channel));

    assert!(matches!(
        provider.post(&at("403"), &Message::text("x")).await,
        Err(ProviderError::Forbidden)
    ));
    assert!(matches!(
        provider.post(&at("404"), &Message::text("x")).await,
        Err(ProviderError::NotFound)
    ));
    assert!(matches!(
        provider
            .post(&at("not-a-snowflake"), &Message::text("x"))
            .await,
        Err(ProviderError::InvalidMessage(_))
    ));
}
