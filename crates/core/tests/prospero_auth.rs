//! Authenticating to prosperod with a bearer token (#46).
//!
//! A stub prosperod records the `Authorization` header of every request, so the
//! tests can check that each route carries the token when one is configured, and
//! that none does when it is not.

use std::sync::{Arc, Mutex};

use ariel_core::prospero::types::SpawnRequest;
use ariel_core::prospero::{ClientError, ProsperoClient};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::json;

const TOKEN: &str = "prospero-token-7c1e-do-not-print";

type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A prosperod that answers every route and records `(path, authorization)`.
/// With `require`, it refuses requests without the right bearer token, as
/// prosperod does with API auth on.
async fn stub(require: Option<&'static str>) -> (ProsperoClient, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .fallback(route)
        .with_state((seen.clone(), require));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        ProsperoClient::new(&format!("http://{addr}")).unwrap(),
        seen,
    )
}

async fn route(
    State((seen, require)): State<(Seen, Option<&'static str>)>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .map(|value| value.to_str().unwrap().to_owned());
    seen.lock()
        .unwrap()
        .push((path.clone(), authorization.clone()));

    if let Some(token) = require {
        match authorization {
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(json!({"error": "unauthorized", "kind": "unauthorized"})),
                )
                    .into_response();
            }
            Some(value) if value != format!("Bearer {token}") => {
                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(json!({"error": "requires scope operate", "kind": "forbidden"})),
                )
                    .into_response();
            }
            Some(_) => {}
        }
    }

    if path.ends_with("/stream") {
        return (
            [(header::CONTENT_TYPE, "text/event-stream")],
            "data: {\"seq\":0,\"ts\":\"t\",\"repo\":\"caliban\",\"agent_id\":\"a1\",\"kind\":{\"kind\":\"agent_spawned\"}}\n\n",
        )
            .into_response();
    }
    let body = if path == "/api/session" {
        if require.is_some() {
            json!({"auth": "token", "token_name": "ariel", "scope": "operate"})
        } else {
            json!({"auth": "disabled"})
        }
    } else if path == "/api/fleet" {
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

/// Call every route the client offers.
async fn exercise(client: &ProsperoClient) {
    client.fleet().await.unwrap();
    client
        .spawn("caliban", &SpawnRequest::new("do the thing"))
        .await
        .unwrap();
    client.kill("a1").await.unwrap();
    client.respawn("a1").await.unwrap();
    client.input("a1", "more").await.unwrap();
    client.end_input("a1").await.unwrap();
    let mut stream = std::pin::pin!(client.stream("a1", 0).await.unwrap());
    stream.next().await.expect("one event").unwrap();
}

#[tokio::test]
async fn every_route_carries_the_bearer_token_when_one_is_configured() {
    let (client, seen) = stub(Some(TOKEN)).await;
    let client = client.with_token(TOKEN);

    exercise(&client).await;

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 7, "every route was called: {seen:?}");
    for (path, authorization) in &seen {
        assert_eq!(
            authorization.as_deref(),
            Some(format!("Bearer {TOKEN}").as_str()),
            "{path} was sent without the token"
        );
    }
}

#[tokio::test]
async fn no_route_carries_a_token_when_none_is_configured() {
    let (client, seen) = stub(None).await;

    exercise(&client).await;

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 7, "{seen:?}");
    assert!(
        seen.iter()
            .all(|(_, authorization)| authorization.is_none()),
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_missing_token_is_an_authentication_error() {
    let (client, _) = stub(Some(TOKEN)).await;

    let error = client.fleet().await.unwrap_err();
    assert!(
        matches!(
            error,
            ClientError::Auth {
                status: StatusCode::UNAUTHORIZED,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(
        error.to_string().contains("prosperod"),
        "the error says who refused: {error}"
    );
}

#[tokio::test]
async fn a_rejected_token_is_an_authentication_error_on_the_stream_too() {
    let (client, _) = stub(Some(TOKEN)).await;
    let client = client.with_token("the-wrong-token");

    let Err(error) = client.stream("a1", 0).await else {
        panic!("the stream should be refused");
    };
    assert!(
        matches!(
            error,
            ClientError::Auth {
                status: StatusCode::FORBIDDEN,
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn the_token_never_appears_in_debug_output_or_errors() {
    let (client, _) = stub(Some(TOKEN)).await;
    let client = client.with_token(TOKEN);
    assert!(
        !format!("{client:?}").contains(TOKEN),
        "Debug leaks the token: {client:?}"
    );

    let (refusing, _) = stub(Some("some-other-token")).await;
    let error = refusing.with_token(TOKEN).fleet().await.unwrap_err();
    assert!(!format!("{error:?}").contains(TOKEN), "{error:?}");
    assert!(!error.to_string().contains(TOKEN), "{error}");
}

#[tokio::test]
async fn session_reports_the_token_prosperod_sees() {
    use ariel_core::prospero::types::{Scope, SessionInfo};

    let (client, _) = stub(Some(TOKEN)).await;
    let session = client.with_token(TOKEN).session().await.unwrap();
    assert_eq!(
        session,
        SessionInfo::Token {
            token_name: "ariel".into(),
            scope: Scope::Operate,
            expires_at: None,
        }
    );
}

#[tokio::test]
async fn session_reports_when_prosperod_has_auth_off() {
    use ariel_core::prospero::types::SessionInfo;

    let (client, _) = stub(None).await;
    assert_eq!(client.session().await.unwrap(), SessionInfo::Disabled);
}

#[test]
fn a_client_still_rejects_an_unusable_base_url() {
    assert!(matches!(
        ProsperoClient::new("not a url"),
        Err(ClientError::BaseUrl { .. })
    ));
}
