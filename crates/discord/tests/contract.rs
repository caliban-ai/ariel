//! `DiscordProvider` against the shared provider contract (ADR 0006), with a
//! stub of Discord's REST API standing in for discord.com.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ariel_core::chat::contract::{self, Harness};
use ariel_core::chat::{Args, ChannelRef, Destination, UserRef};
use ariel_discord::{DiscordConfig, DiscordProvider};
use axum::Router;
use axum::extract::State;
use axum::routing::{patch, post, put};
use serde_json::{Value, json};

const GUILD: &str = "100";
const CHANNEL: &str = "200";
const USER: &str = "300";
const APP: &str = "400";

/// Hands out increasing snowflake IDs, as Discord would.
#[derive(Default)]
struct Ids(AtomicU64);

impl Ids {
    fn next(&self) -> String {
        (1_000 + self.0.fetch_add(1, Ordering::SeqCst)).to_string()
    }
}

fn message(ids: &Ids, channel: &str) -> Value {
    json!({
        "id": ids.next(), "channel_id": channel, "content": "", "embeds": [],
        "attachments": [], "components": [], "mentions": [], "mention_roles": [],
        "mention_everyone": false, "pinned": false, "tts": false, "type": 0,
        "timestamp": "2026-09-13T00:00:00+00:00",
        "author": { "id": APP, "username": "ariel", "discriminator": "0", "avatar": null }
    })
}

async fn stub() -> std::net::SocketAddr {
    let ids = Arc::new(Ids::default());
    let app = Router::new()
        .route(
            "/api/v10/channels/{channel}/messages",
            post(|State(ids): State<Arc<Ids>>, axum::extract::Path(channel): axum::extract::Path<String>| async move {
                axum::Json(message(&ids, &channel))
            }),
        )
        .route(
            "/api/v10/channels/{channel}/messages/{message}",
            patch(|State(ids): State<Arc<Ids>>, axum::extract::Path((channel, _m)): axum::extract::Path<(String, String)>| async move {
                axum::Json(message(&ids, &channel))
            }),
        )
        .route(
            "/api/v10/channels/{channel}/messages/{message}/threads",
            post(|axum::extract::Path((channel, message)): axum::extract::Path<(String, String)>| async move {
                axum::Json(json!({
                    "id": message, "type": 11, "guild_id": GUILD, "parent_id": channel,
                    "name": "thread"
                }))
            }),
        )
        .route(
            "/api/v10/users/@me/channels",
            post(|| async { axum::Json(json!({ "id": "250", "type": 1 })) }),
        )
        .route(
            "/api/v10/applications/{app}/guilds/{guild}/commands",
            put(|| async { axum::Json(json!([])) }),
        )
        .route(
            "/api/v10/interactions/{id}/{token}/callback",
            post(|| async { axum::http::StatusCode::NO_CONTENT }),
        )
        .route(
            "/api/v10/webhooks/{app}/{token}",
            post(|State(ids): State<Arc<Ids>>| async move { axum::Json(message(&ids, CHANNEL)) }),
        )
        .route(
            "/api/v10/webhooks/{app}/{token}/messages/@original",
            patch(|State(ids): State<Arc<Ids>>| async move { axum::Json(message(&ids, CHANNEL)) }),
        )
        .with_state(ids);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Delivers slash commands as Gateway interactions would arrive.
struct DiscordHarness {
    provider: Arc<DiscordProvider>,
    next: AtomicU64,
}

impl Harness for DiscordHarness {
    fn destination(&self) -> Destination {
        Destination::Channel(ChannelRef::new("discord", GUILD, CHANNEL))
    }

    fn user(&self) -> UserRef {
        UserRef::new("discord", GUILD, USER)
    }

    fn inject_command(&self, name: &str, _args: Args) {
        let id = 5_000 + self.next.fetch_add(1, Ordering::SeqCst);
        let interaction = json!({
            "id": id.to_string(), "application_id": APP, "type": 2, "token": format!("tok-{id}"),
            "guild_id": GUILD, "channel": { "id": CHANNEL, "type": 0 },
            "authorizing_integration_owners": {}, "entitlements": [],
            "member": { "user": { "id": USER, "username": "someone", "discriminator": "0", "avatar": null },
                        "roles": [], "joined_at": "2026-01-01T00:00:00+00:00", "deaf": false, "mute": false, "flags": 0 },
            "data": { "id": "600", "name": "ariel", "type": 1,
                      "options": [{ "name": name, "type": 1, "options": [] }] }
        });
        self.provider.deliver_interaction(
            serde_json::from_value(interaction).expect("valid interaction payload"),
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn discord_provider_satisfies_the_contract() {
    let addr = stub().await;
    let provider = Arc::new(
        DiscordProvider::new(DiscordConfig {
            token: "test-token".to_owned(),
            guild_id: GUILD.parse().unwrap(),
            application_id: APP.parse().unwrap(),
            api_proxy: Some(addr.to_string()),
        })
        .expect("build provider"),
    );
    let harness = DiscordHarness {
        provider: Arc::clone(&provider),
        next: AtomicU64::new(0),
    };

    assert_eq!(contract::check(provider.as_ref(), &harness).await, []);
}
