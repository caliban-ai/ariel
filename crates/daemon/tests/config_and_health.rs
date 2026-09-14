//! `arield` startup: credentials from files, and the health endpoint (#30,
//! ADR 0008).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;

use ariel_daemon::config::{Config, ConfigError, Secret};
use ariel_daemon::health;

const TOKEN: &str = "tok-7f3a-do-not-print";

/// A file under the system temp dir, unique to this test and process.
fn temp_file(test: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ariel-{test}-{}", std::process::id()));
    std::fs::write(&path, contents).unwrap();
    path
}

fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

#[test]
fn token_files_are_read_with_one_trailing_newline_removed() {
    let discord = temp_file("discord", &format!("{TOKEN}\n"));
    let gonzalo = temp_file("gonzalo", "gz-token");

    let config = Config::from_lookup(lookup(&[
        ("ARIEL_DISCORD_TOKEN_FILE", discord.to_str().unwrap()),
        ("ARIEL_GONZALO_TOKEN_FILE", gonzalo.to_str().unwrap()),
    ]))
    .unwrap();

    assert_eq!(
        config.discord_token.as_ref().map(Secret::expose),
        Some(TOKEN)
    );
    assert_eq!(
        config.gonzalo_token.as_ref().map(Secret::expose),
        Some("gz-token")
    );
}

#[test]
fn unset_token_variables_mean_no_token_and_the_default_health_address() {
    let config = Config::from_lookup(lookup(&[])).unwrap();

    assert!(config.discord_token.is_none());
    assert!(config.gonzalo_token.is_none());
    assert_eq!(
        config.health_addr,
        "0.0.0.0:8081".parse::<SocketAddr>().unwrap()
    );
}

#[test]
fn health_address_can_be_overridden_and_must_parse() {
    let config = Config::from_lookup(lookup(&[("ARIEL_HEALTH_ADDR", "127.0.0.1:9000")])).unwrap();
    assert_eq!(
        config.health_addr,
        "127.0.0.1:9000".parse::<SocketAddr>().unwrap()
    );

    let error =
        Config::from_lookup(lookup(&[("ARIEL_HEALTH_ADDR", "not-an-address")])).unwrap_err();
    assert!(matches!(error, ConfigError::HealthAddr { .. }));
    assert!(error.to_string().contains("ARIEL_HEALTH_ADDR"));
}

#[test]
fn a_missing_token_file_is_an_error_naming_the_variable_and_path() {
    let missing = std::env::temp_dir().join("ariel-definitely-missing-token-file");

    let error = Config::from_lookup(lookup(&[(
        "ARIEL_DISCORD_TOKEN_FILE",
        missing.to_str().unwrap(),
    )]))
    .unwrap_err();

    assert!(matches!(error, ConfigError::Unreadable { .. }));
    let message = error.to_string();
    assert!(message.contains("ARIEL_DISCORD_TOKEN_FILE"), "{message}");
    assert!(
        message.contains("ariel-definitely-missing-token-file"),
        "{message}"
    );
}

#[test]
fn an_empty_token_file_is_an_error() {
    let empty = temp_file("empty", "\n");

    let error = Config::from_lookup(lookup(&[(
        "ARIEL_GONZALO_TOKEN_FILE",
        empty.to_str().unwrap(),
    )]))
    .unwrap_err();

    assert!(matches!(error, ConfigError::Empty { .. }));
    assert!(error.to_string().contains("ARIEL_GONZALO_TOKEN_FILE"));
}

#[test]
fn tokens_are_redacted_when_formatted() {
    let file = temp_file("redacted", TOKEN);
    let config = Config::from_lookup(lookup(&[(
        "ARIEL_DISCORD_TOKEN_FILE",
        file.to_str().unwrap(),
    )]))
    .unwrap();
    let secret = config.discord_token.as_ref().unwrap();

    for rendered in [
        format!("{secret:?}"),
        format!("{secret}"),
        format!("{config:?}"),
    ] {
        assert!(!rendered.contains(TOKEN), "token leaked: {rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }
}

#[tokio::test]
async fn healthz_answers_ok_and_other_paths_are_not_found() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(health::serve(listener));

    let ok = reqwest::get(format!("http://{addr}/healthz"))
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    assert_eq!(ok.text().await.unwrap(), "ok");

    let other = reqwest::get(format!("http://{addr}/nope")).await.unwrap();
    assert_eq!(other.status().as_u16(), 404);
}

#[test]
fn arield_exits_non_zero_on_a_bad_token_file_without_printing_tokens() {
    let discord = temp_file("process", TOKEN);
    let missing = std::env::temp_dir().join("ariel-missing-gonzalo-token");

    let out = Command::new(env!("CARGO_BIN_EXE_arield"))
        .env_clear()
        .env("ARIEL_DISCORD_TOKEN_FILE", &discord)
        .env("ARIEL_GONZALO_TOKEN_FILE", &missing)
        .env("ARIEL_HEALTH_ADDR", "127.0.0.1:0")
        .output()
        .expect("run arield");

    assert!(!out.status.success(), "arield exited {}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ARIEL_GONZALO_TOKEN_FILE"),
        "stderr: {stderr}"
    );
    assert!(
        !stdout.contains(TOKEN) && !stderr.contains(TOKEN),
        "token printed"
    );
}
