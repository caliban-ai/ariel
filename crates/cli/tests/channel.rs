//! `ariel channel` against a local gonzalo store (#18).

use std::path::Path;
use std::process::{Command, Output};

use gonzalo::{KeyPrefix, Store};

/// Run `ariel` against the store in `dir`.
fn ariel(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ariel"))
        .arg("channel")
        .args(args)
        .arg("--store")
        .arg(dir)
        .output()
        .expect("run ariel channel")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("utf-8 stderr")
}

const CHANNEL: [&str; 6] = [
    "--provider",
    "discord",
    "--tenant",
    "guild-1",
    "--channel",
    "ops",
];

fn set(dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["set"];
    args.extend_from_slice(&CHANNEL);
    args.extend_from_slice(extra);
    ariel(dir, &args)
}

fn show(dir: &Path) -> Output {
    let mut args = vec!["show"];
    args.extend_from_slice(&CHANNEL);
    ariel(dir, &args)
}

/// Every audit entry in the store, as `action result`.
async fn audit(dir: &Path) -> Vec<String> {
    let store = gonzalo::FsStore::new(dir);
    let keys = store
        .list(&KeyPrefix {
            namespace: Some("fleet-audit".to_owned()),
            collection: Some("entries".to_owned()),
        })
        .await
        .unwrap();
    let mut entries = Vec::new();
    for key in keys {
        let record = store.get(&key).await.unwrap().unwrap();
        let entry: serde_json::Value =
            serde_json::from_slice(record.body.bytes()).expect("an audit entry");
        entries.push(format!("{} {}", entry["action"], entry["result"]));
    }
    entries
}

#[tokio::test]
async fn set_then_show_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let created = set(
        dir.path(),
        &["--follows", "caliban,prospero", "--ceiling", "operator"],
    );
    assert!(created.status.success(), "{}", stderr(&created));
    assert!(
        stdout(&created).contains("created"),
        "says what it did: {}",
        stdout(&created)
    );

    let shown = show(dir.path());
    assert!(shown.status.success(), "{}", stderr(&shown));
    let text = stdout(&shown);
    assert!(text.contains("discord/guild-1/ops"), "{text}");
    assert!(text.contains("caliban"), "{text}");
    assert!(text.contains("prospero"), "{text}");
    assert!(text.contains("operator"), "{text}");
    // The default preset, since --notify was not given.
    assert!(text.contains("all"), "{text}");
}

#[tokio::test]
async fn a_channel_can_follow_the_whole_fleet() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        set(dir.path(), &["--follows", "fleet"]).status.success(),
        "following the fleet is spelled `fleet`"
    );
    let text = stdout(&show(dir.path()));
    assert!(text.contains("fleet"), "{text}");
    // A brand new channel is read-only until its ceiling is raised (ADR 0009).
    assert!(text.contains("viewer"), "{text}");
}

#[tokio::test]
async fn showing_an_unconfigured_channel_fails_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let out = show(dir.path());
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("not configured"), "{}", stderr(&out));
}

#[tokio::test]
async fn a_new_channel_without_follows_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = set(dir.path(), &["--ceiling", "admin"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("follows"), "{}", stderr(&out));
    assert!(audit(dir.path()).await.is_empty(), "nothing was written");
}

#[tokio::test]
async fn changing_the_ceiling_is_audited() {
    let dir = tempfile::tempdir().unwrap();
    set(dir.path(), &["--follows", "fleet"]);
    let raised = set(dir.path(), &["--ceiling", "admin"]);
    assert!(raised.status.success(), "{}", stderr(&raised));
    assert!(
        stdout(&raised).contains("viewer") && stdout(&raised).contains("admin"),
        "an update reports what changed: {}",
        stdout(&raised)
    );

    let entries = audit(dir.path()).await;
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(
        entries.iter().any(|entry| entry.contains("channel.update")),
        "{entries:?}"
    );
}

#[tokio::test]
async fn setting_what_is_already_stored_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    set(dir.path(), &["--follows", "fleet"]);
    let again = set(dir.path(), &["--follows", "fleet"]);

    assert!(again.status.success(), "{}", stderr(&again));
    assert!(stdout(&again).contains("unchanged"), "{}", stdout(&again));
    assert_eq!(audit(dir.path()).await.len(), 1, "only the create");
}

#[tokio::test]
async fn an_empty_workspace_list_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = set(dir.path(), &["--follows", ""]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("workspace"),
        "names what is wrong: {}",
        stderr(&out)
    );
}

#[test]
fn channel_help_lists_the_subcommands() {
    let out = Command::new(env!("CARGO_BIN_EXE_ariel"))
        .args(["channel", "--help"])
        .output()
        .expect("run ariel channel --help");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("show"), "{text}");
    assert!(text.contains("set"), "{text}");
}
