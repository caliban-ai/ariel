//! Account linking: minting a one-time token and redeeming it in chat (#16).
//!
//! Only a hash of a token is ever stored (gonzalo ADR 0022), a token works once,
//! and every redemption attempt is audited.

use std::sync::Arc;
use std::time::Duration;

use ariel_core::chat::UserRef;
use ariel_core::link::{self, LinkError, MintRequest};
use ariel_core::records::Records;
use ariel_core::records::gonzalo::{
    AuditEntry, AuditResult, Authenticator, FleetActor, FleetRole, FsStore, GrantScope, Identity,
    IdentityBinding, KeyPrefix, LinkToken, Person, RoleGrant, Store,
};
use tempfile::TempDir;

const NOW: i64 = 1_760_000_000_000;

fn records() -> (TempDir, Records) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FsStore::new(dir.path()));
    (dir, Records::new(store, Identity::new("ariel-test")))
}

fn admin() -> FleetActor {
    FleetActor::Service("ariel-cli".into())
}

fn account(user: &str) -> UserRef {
    UserRef::new("discord", "guild-1", user)
}

fn operator_token() -> MintRequest {
    MintRequest {
        role: FleetRole::Operator,
        scope: GrantScope::Fleet,
        person: None,
        ttl: Duration::from_secs(600),
        minted_by: admin(),
    }
}

async fn audit_entries(records: &Records) -> Vec<AuditEntry> {
    let mut entries = Vec::new();
    for key in records.list::<AuditEntry>().await.unwrap() {
        entries.push(
            records
                .get::<AuditEntry>(&key)
                .await
                .unwrap()
                .unwrap()
                .value,
        );
    }
    entries
}

#[tokio::test]
async fn a_redeemed_token_links_the_account_and_grants_the_role() {
    let (_dir, records) = records();
    let minted = link::mint(&records, operator_token(), NOW).await.unwrap();

    let linked = link::redeem(
        &records,
        &minted.token,
        &account("user-1"),
        Some("ada"),
        NOW + 1,
    )
    .await
    .unwrap();
    assert_eq!(linked.role, FleetRole::Operator);
    assert_eq!(linked.scope, GrantScope::Fleet);

    // The binding points the account at the person...
    let binding_key = IdentityBinding::key_for(&Authenticator::Discord, "user-1").unwrap();
    let binding = records
        .get::<IdentityBinding>(&binding_key)
        .await
        .unwrap()
        .expect("the account is bound");
    assert_eq!(binding.value.person, linked.person);
    assert_eq!(binding.value.handle.as_deref(), Some("ada"));

    // ...the person exists...
    let person = records
        .get::<Person>(&Person::key(&linked.person).unwrap())
        .await
        .unwrap()
        .expect("a new person was created");
    assert_eq!(person.value.display_name, "ada");

    // ...and holds the token's role.
    let grant = records
        .get::<RoleGrant>(&RoleGrant::key_for(&linked.person, &GrantScope::Fleet).unwrap())
        .await
        .unwrap()
        .expect("the role was granted");
    assert_eq!(grant.value.role, FleetRole::Operator);
    assert_eq!(grant.value.granted_by, admin(), "the minter granted it");
}

#[tokio::test]
async fn a_token_works_only_once() {
    let (_dir, records) = records();
    let minted = link::mint(&records, operator_token(), NOW).await.unwrap();

    link::redeem(&records, &minted.token, &account("user-1"), None, NOW + 1)
        .await
        .unwrap();
    let second = link::redeem(&records, &minted.token, &account("user-2"), None, NOW + 2).await;
    assert!(matches!(second, Err(LinkError::AlreadyUsed)), "{second:?}");
}

#[tokio::test]
async fn two_concurrent_redemptions_of_one_token_give_exactly_one_success() {
    let (_dir, records) = records();
    let minted = link::mint(&records, operator_token(), NOW).await.unwrap();
    let (first_account, second_account) = (account("user-1"), account("user-2"));

    let (first, second) = tokio::join!(
        link::redeem(&records, &minted.token, &first_account, None, NOW + 1),
        link::redeem(&records, &minted.token, &second_account, None, NOW + 1),
    );

    let successes = [&first, &second].iter().filter(|r| r.is_ok()).count();
    assert_eq!(successes, 1, "{first:?} / {second:?}");
    let failure = if first.is_ok() { &second } else { &first };
    assert!(
        matches!(failure, Err(LinkError::AlreadyUsed)),
        "the loser is told the token is used: {failure:?}"
    );

    let bindings = records.list::<IdentityBinding>().await.unwrap();
    assert_eq!(bindings.len(), 1, "exactly one account was linked");
}

#[tokio::test]
async fn an_expired_token_is_rejected_and_links_nothing() {
    let (_dir, records) = records();
    let minted = link::mint(&records, operator_token(), NOW).await.unwrap();

    let late = NOW + Duration::from_secs(601).as_millis() as i64;
    let result = link::redeem(&records, &minted.token, &account("user-1"), None, late).await;
    assert!(matches!(result, Err(LinkError::Expired)), "{result:?}");
    assert!(records.list::<IdentityBinding>().await.unwrap().is_empty());
    assert!(records.list::<RoleGrant>().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unknown_or_malformed_token_is_rejected_the_same_way() {
    let (_dir, records) = records();
    link::mint(&records, operator_token(), NOW).await.unwrap();

    let unknown = link::redeem(&records, &"ab".repeat(32), &account("user-1"), None, NOW).await;
    assert!(matches!(unknown, Err(LinkError::Invalid)), "{unknown:?}");

    let malformed = link::redeem(&records, "not-a-token", &account("user-1"), None, NOW).await;
    assert!(
        matches!(malformed, Err(LinkError::Invalid)),
        "a malformed token is not distinguished from an unknown one: {malformed:?}"
    );
}

#[tokio::test]
async fn an_already_linked_account_does_not_burn_a_token() {
    let (_dir, records) = records();
    let first = link::mint(&records, operator_token(), NOW).await.unwrap();
    link::redeem(&records, &first.token, &account("user-1"), None, NOW + 1)
        .await
        .unwrap();

    let second = link::mint(&records, operator_token(), NOW).await.unwrap();
    let again = link::redeem(&records, &second.token, &account("user-1"), None, NOW + 2).await;
    assert!(matches!(again, Err(LinkError::AlreadyLinked)), "{again:?}");

    // The second token is still usable by someone else.
    link::redeem(&records, &second.token, &account("user-2"), None, NOW + 3)
        .await
        .expect("the refused attempt did not consume the token");
}

#[tokio::test]
async fn a_token_minted_for_a_person_links_to_that_person() {
    let (_dir, records) = records();
    let request = MintRequest {
        person: Some("p-ada".into()),
        scope: GrantScope::Workspace("caliban".into()),
        ..operator_token()
    };
    let minted = link::mint(&records, request, NOW).await.unwrap();

    let linked = link::redeem(&records, &minted.token, &account("user-1"), None, NOW + 1)
        .await
        .unwrap();
    assert_eq!(linked.person, "p-ada");
    assert_eq!(linked.scope, GrantScope::Workspace("caliban".into()));
}

#[tokio::test]
async fn the_raw_token_is_never_persisted() {
    let (dir, records) = records();
    let minted = link::mint(&records, operator_token(), NOW).await.unwrap();
    link::redeem(
        &records,
        &minted.token,
        &account("user-1"),
        Some("ada"),
        NOW + 1,
    )
    .await
    .unwrap();
    // A failed attempt with the same token, which must not leak it either.
    let _ = link::redeem(&records, &minted.token, &account("user-2"), None, NOW + 2).await;

    // The stored token record holds a hash, not the token.
    let stored = records
        .get::<LinkToken>(&minted.key)
        .await
        .unwrap()
        .expect("the token record exists");
    assert_ne!(stored.value.token_hash, minted.token);
    assert!(stored.value.consumed.is_some(), "consumed, not deleted");

    // No record anywhere in the store holds it: not a body, not a key. This
    // reads through the store rather than grepping its files, because a body's
    // on-disk encoding is the store's business; a file grep once let a leak
    // into an audit entry pass unnoticed.
    let store = FsStore::new(dir.path());
    let keys = store.list(&KeyPrefix::default()).await.unwrap();
    assert!(
        keys.len() >= 6,
        "token, person, binding, grant and audit entries were all scanned: {keys:?}"
    );
    for key in keys {
        assert!(
            !key.to_string().contains(&minted.token),
            "{key} names the raw token"
        );
        let record = store
            .get(&key)
            .await
            .unwrap()
            .expect("listed record exists");
        let body = String::from_utf8_lossy(record.body.bytes());
        assert!(
            !body.contains(&minted.token),
            "{key} contains the raw token"
        );
    }
}

#[tokio::test]
async fn successful_and_failed_redemptions_are_both_audited() {
    let (_dir, records) = records();
    let minted = link::mint(&records, operator_token(), NOW).await.unwrap();
    let minted_entries = audit_entries(&records).await.len();

    link::redeem(&records, &minted.token, &account("user-1"), None, NOW + 1)
        .await
        .unwrap();
    let _ = link::redeem(&records, &minted.token, &account("user-2"), None, NOW + 2).await;
    let _ = link::redeem(&records, "garbage", &account("user-3"), None, NOW + 3).await;

    let entries = audit_entries(&records).await;
    let redemptions: Vec<_> = entries
        .iter()
        .filter(|entry| entry.action == "link.redeem")
        .collect();
    assert_eq!(
        redemptions.len(),
        3,
        "every attempt is audited: {entries:?}"
    );
    assert_eq!(
        redemptions
            .iter()
            .filter(|entry| entry.result == AuditResult::Succeeded)
            .count(),
        1
    );
    assert!(
        redemptions.iter().any(|entry| entry.actor
            == FleetActor::Unlinked {
                authenticator: Authenticator::Discord,
                subject: "user-3".into(),
            }),
        "a failed attempt names the account that tried: {redemptions:?}"
    );
    assert!(
        entries.len() > minted_entries,
        "minting is audited too, before any redemption"
    );
}
