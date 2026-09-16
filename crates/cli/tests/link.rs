//! `ariel link new` against a local gonzalo store (#16).

use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use ariel_core::chat::UserRef;
use ariel_core::link;
use ariel_core::records::Records;
use ariel_core::records::gonzalo::{FleetRole, FsStore, GrantScope, Identity, KeyPrefix, Store};

fn mint(dir: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ariel"))
        .args(["link", "new"])
        .args(extra)
        .arg("--store")
        .arg(dir)
        .output()
        .expect("run ariel link new")
}

/// The 64-hex-character token in the command's output.
fn token(out: &Output) -> String {
    let text = String::from_utf8(out.stdout.clone()).unwrap();
    text.split_whitespace()
        .find(|word| word.len() == 64 && word.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or_else(|| panic!("no token in output: {text}"))
        .to_owned()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test]
async fn a_minted_token_can_be_redeemed_for_its_role() {
    let dir = tempfile::tempdir().unwrap();
    let out = mint(
        dir.path(),
        &["--role", "operator", "--workspace", "caliban"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("/ariel link"),
        "says how to redeem: {stdout}"
    );
    assert!(
        stdout.contains("shown once"),
        "warns it is shown once: {stdout}"
    );

    let records = Records::new(
        Arc::new(FsStore::new(dir.path())),
        Identity::new("ariel-test"),
    );
    let linked = link::redeem(
        &records,
        &token(&out),
        &UserRef::new("discord", "guild-1", "user-1"),
        None,
        now_ms(),
    )
    .await
    .expect("the printed token redeems");
    assert_eq!(linked.role, FleetRole::Operator);
    assert_eq!(linked.scope, GrantScope::Workspace("caliban".into()));
}

#[tokio::test]
async fn the_minted_token_is_not_stored() {
    let dir = tempfile::tempdir().unwrap();
    let out = mint(dir.path(), &["--role", "admin"]);
    assert!(out.status.success());
    let token = token(&out);

    let store = FsStore::new(dir.path());
    let keys = store.list(&KeyPrefix::default()).await.unwrap();
    assert!(!keys.is_empty(), "the token hash and its audit entry exist");
    for key in keys {
        assert!(!key.to_string().contains(&token), "{key} names the token");
        let record = store.get(&key).await.unwrap().unwrap();
        assert!(
            !String::from_utf8_lossy(record.body.bytes()).contains(&token),
            "{key} contains the token"
        );
    }
}

#[test]
fn a_role_is_required() {
    let dir = tempfile::tempdir().unwrap();
    let out = mint(dir.path(), &[]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--role"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
