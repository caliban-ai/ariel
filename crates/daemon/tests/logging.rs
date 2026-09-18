//! `arield` logs to stderr (#49): startup lines appear without configuration,
//! a refused prosperod token is a visible error naming the variable to fix,
//! `RUST_LOG` changes the level, and the token itself is never written.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use serde_json::json;

const TOKEN: &str = "pspo_logging-test-do-not-print";

/// A prosperod that refuses every token.
async fn refusing_prosperod() -> String {
    let app = Router::new().route(
        "/api/session",
        get(|| async {
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "unauthorized", "kind": "unauthorized"})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn token_file(test: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ariel-log-{test}-{}", std::process::id()));
    std::fs::write(&path, format!("{TOKEN}\n")).unwrap();
    path
}

/// `arield` with only prosperod configured, and `extra` on top.
fn spawn_arield(prospero: &str, token: &PathBuf, extra: &[(&str, &str)]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arield"));
    command
        .env_clear()
        .env("ARIEL_PROSPERO_URL", prospero)
        .env("ARIEL_PROSPERO_TOKEN_FILE", token)
        .env("ARIEL_HEALTH_ADDR", "127.0.0.1:0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (name, value) in extra {
        command.env(name, value);
    }
    command.spawn().expect("spawn arield")
}

/// Everything `arield` writes to stderr up to and including the first line
/// containing `until`, or everything within the timeout. Stops the process.
fn stderr_until(mut child: Child, until: &str) -> String {
    let stderr = child.stderr.take().unwrap();
    let (lines, received) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if lines.send(line.unwrap()).is_err() {
                break;
            }
        }
    });

    let mut out = String::new();
    while let Ok(line) = received.recv_timeout(Duration::from_secs(10)) {
        out.push_str(&line);
        out.push('\n');
        if line.contains(until) {
            break;
        }
    }
    child.kill().ok();
    child.wait().ok();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_token_is_logged_naming_the_variable_but_never_the_token() {
    let prospero = refusing_prosperod().await;
    let token = token_file("refused");

    let out = stderr_until(spawn_arield(&prospero, &token, &[]), "refused");

    assert!(out.contains("arield starting"), "{out}");
    assert!(out.contains("serving health checks"), "{out}");
    let refused = out
        .lines()
        .find(|line| line.contains("refused"))
        .unwrap_or_else(|| panic!("no refused-token line in:\n{out}"));
    assert!(refused.contains("ERROR"), "{refused}");
    assert!(refused.contains("ARIEL_PROSPERO_TOKEN_FILE"), "{refused}");
    assert!(!out.contains(TOKEN), "the token reached the log:\n{out}");
    assert!(
        !out.contains("logging-test"),
        "part of the token reached the log:\n{out}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_log_hides_startup_lines_below_its_level() {
    let prospero = refusing_prosperod().await;
    let token = token_file("quiet");

    let out = stderr_until(
        spawn_arield(&prospero, &token, &[("RUST_LOG", "error")]),
        "refused",
    );

    assert!(out.contains("refused"), "{out}");
    assert!(!out.contains("arield starting"), "{out}");
    assert!(!out.contains("serving health only"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn json_format_writes_parseable_lines() {
    let prospero = refusing_prosperod().await;
    let token = token_file("json");

    let out = stderr_until(
        spawn_arield(&prospero, &token, &[("ARIEL_LOG_FORMAT", "json")]),
        "refused",
    );

    let events: Vec<serde_json::Value> = out
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["message"] == "arield starting"),
        "{out}"
    );
    assert!(!out.contains(TOKEN), "{out}");
}

#[test]
fn an_unknown_log_format_stops_arield_naming_the_variable() {
    let out = Command::new(env!("CARGO_BIN_EXE_arield"))
        .env_clear()
        .env("ARIEL_LOG_FORMAT", "yaml")
        .output()
        .expect("run arield");

    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("ARIEL_LOG_FORMAT"), "{stderr}");
}
