//! Telling prosperod which person a command is for (#59, prospero#251).
//!
//! Ariel holds one `operate` token for a whole Discord, so prosperod's `actor`
//! names Ariel on every chat spawn. A stub prosperod records the
//! `X-Prospero-On-Behalf-Of` header of every request, so these tests can check
//! that a client acting for someone sends it, that a plain client sends
//! nothing, and that a value prosperod would answer `400` to never leaves.

use std::sync::{Arc, Mutex};

use ariel_core::prospero::ProsperoClient;
use ariel_core::prospero::types::SpawnRequest;
use axum::Router;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::json;

const HEADER: &str = "x-prospero-on-behalf-of";

/// `(path, on-behalf-of)` for every request the client made.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

async fn stub() -> (ProsperoClient, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new().fallback(route).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        ProsperoClient::new(&format!("http://{addr}")).unwrap(),
        seen,
    )
}

async fn route(State(seen): State<Seen>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let subject = request
        .headers()
        .get(HEADER)
        .map(|value| value.to_str().unwrap().to_owned());
    seen.lock().unwrap().push((path.clone(), subject));

    if path.ends_with("/stream") {
        return (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            "data: {\"seq\":0,\"ts\":\"t\",\"repo\":\"caliban\",\"agent_id\":\"a1\",\"kind\":{\"kind\":\"agent_spawned\"}}\n\n",
        )
            .into_response();
    }
    let body = if path == "/api/fleet" {
        json!({"host": "stub", "workspaces": []})
    } else if path.ends_with("/agents") {
        json!({"agent_id": "a2", "workspace": "caliban", "isolated": false, "created": true})
    } else if path.ends_with("/respawn") {
        json!({"agent_id": "a3"})
    } else {
        json!({})
    };
    axum::Json(body).into_response()
}

/// Every route a command can reach.
async fn exercise(client: &ProsperoClient) {
    client.fleet().await.unwrap();
    client
        .spawn("caliban", &SpawnRequest::new("do the thing"))
        .await
        .unwrap();
    client.kill("a1").await.unwrap();
    client.respawn("a1").await.unwrap();
    let mut stream = std::pin::pin!(client.stream("a1", 0).await.unwrap());
    stream.next().await.expect("one event").unwrap();
}

#[tokio::test]
async fn a_client_acting_for_someone_names_them_on_every_request() {
    let (client, seen) = stub().await;

    exercise(&client.on_behalf_of("ada")).await;

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 5, "every route was called: {seen:?}");
    for (path, subject) in &seen {
        assert_eq!(subject.as_deref(), Some("ada"), "{path} did not name them");
    }
}

#[tokio::test]
async fn a_plain_client_names_nobody() {
    let (client, seen) = stub().await;

    exercise(&client).await;

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 5);
    for (path, subject) in &seen {
        assert_eq!(subject, &None, "{path} asserted an attribution");
    }
}

/// The original client is unchanged, so one person's command cannot leak into
/// the next.
#[tokio::test]
async fn acting_for_someone_does_not_change_the_client_it_came_from() {
    let (client, seen) = stub().await;

    client.on_behalf_of("ada").fleet().await.unwrap();
    client.fleet().await.unwrap();
    client.on_behalf_of("grace").fleet().await.unwrap();

    let subjects: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, subject)| subject.clone())
        .collect();
    assert_eq!(
        subjects,
        vec![Some("ada".to_owned()), None, Some("grace".to_owned())]
    );
}

/// prosperod answers `400` to a blank value, one over 128 characters, or one
/// carrying control characters. None of those can be a gonzalo person id, but
/// sending one would turn a working command into a failed request, so the
/// client drops it instead.
#[tokio::test]
async fn a_value_prosperod_would_refuse_is_never_sent() {
    let refused = [
        ("", "blank"),
        ("   ", "whitespace only"),
        ("ada\nactor=root", "a forged log line"),
        ("ada\u{7f}", "a control character"),
    ];
    for (subject, why) in refused {
        let (client, seen) = stub().await;

        client.on_behalf_of(subject).fleet().await.unwrap();

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[0].1, None, "{why} was sent as an attribution");
    }

    let (client, seen) = stub().await;
    let too_long = "a".repeat(129);
    client.on_behalf_of(&too_long).fleet().await.unwrap();
    assert_eq!(
        seen.lock().unwrap()[0].1,
        None,
        "a 129-character value was sent"
    );

    // The boundary itself is fine.
    let (client, seen) = stub().await;
    let longest = "a".repeat(128);
    client.on_behalf_of(&longest).fleet().await.unwrap();
    assert_eq!(seen.lock().unwrap()[0].1, Some(longest));
}

/// prosperod trims the value before recording it, so the client sends what
/// will actually be stored rather than something that only looks different.
#[tokio::test]
async fn a_padded_value_is_trimmed_the_way_prosperod_trims_it() {
    let (client, seen) = stub().await;

    client.on_behalf_of("  ada  ").fleet().await.unwrap();

    assert_eq!(seen.lock().unwrap()[0].1.as_deref(), Some("ada"));
}
