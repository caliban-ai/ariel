//! The gonzalo client for access-control records (#15), against `FsStore` in a
//! temporary directory.

use std::sync::Arc;

use ariel_core::records::gonzalo::{
    AuditEntry, AuditResult, Authenticator, BindingOrigin, ChannelConfig, FleetActor, FleetRole,
    Follows, FsStore, GrantScope, Identity, IdentityBinding, LinkSecret, LinkToken, NotifyPreset,
    Person, RecordKey, RoleGrant, VerifiedEmail,
};
use ariel_core::records::{Delete, FleetRecord, Records, RecordsError, Write};
use tempfile::TempDir;

const NOW: i64 = 1_760_000_000_000;

fn records() -> (TempDir, Records) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FsStore::new(dir.path()));
    (dir, Records::new(store, Identity::new("ariel-test")))
}

fn person() -> Person {
    Person {
        display_name: "Ada".into(),
        email: Some("ada@example.com".into()),
    }
}

fn binding() -> IdentityBinding {
    IdentityBinding {
        authenticator: Authenticator::Discord,
        subject: "1234".into(),
        person: "p1".into(),
        handle: Some("ada".into()),
        email: Some(VerifiedEmail {
            address: "ada@example.com".into(),
            verified: true,
        }),
        bound_at: NOW,
        bound_by: BindingOrigin::Operator(FleetActor::Service("ariel-cli".into())),
    }
}

fn grant() -> RoleGrant {
    RoleGrant {
        person: "p1".into(),
        scope: GrantScope::Workspace("caliban".into()),
        role: FleetRole::Operator,
        granted_by: FleetActor::Person("p0".into()),
        granted_at: NOW,
    }
}

fn channel() -> ChannelConfig {
    ChannelConfig {
        provider: "discord".into(),
        tenant: "guild-1".into(),
        channel: "5678".into(),
        follows: Follows::workspaces(["caliban"]).unwrap(),
        notify: NotifyPreset::Failures,
        ceiling: FleetRole::Viewer,
    }
}

fn token() -> LinkToken {
    LinkToken::new(
        &LinkSecret::from_bytes([7; 32]),
        FleetRole::Operator,
        GrantScope::Fleet,
        None,
        FleetActor::Service("ariel-cli".into()),
        NOW,
        NOW + 600_000,
    )
}

fn audit() -> AuditEntry {
    AuditEntry {
        actor: FleetActor::Person("p1".into()),
        action: "spawn".into(),
        target: "caliban-ai/caliban".into(),
        at: NOW,
        surface_ref: Some("discord:5678:9999".into()),
        result: AuditResult::Denied,
    }
}

/// Creates `value` at `key`, reads it back, and checks it is unchanged.
async fn round_trip<T>(records: &Records, key: RecordKey, value: T)
where
    T: FleetRecord + PartialEq + std::fmt::Debug + Clone,
{
    let written = match records.create(key.clone(), value.clone()).await.unwrap() {
        Write::Committed(v) => v,
        Write::Conflict(current) => panic!("create conflicted with {current:?}"),
    };
    assert_eq!(written.key, key);
    assert_eq!(written.value, value);

    let read = records
        .get::<T>(&key)
        .await
        .unwrap()
        .expect("record exists");
    assert_eq!(read.value, value);
    assert_eq!(read.revision, written.revision);
}

#[tokio::test]
async fn every_record_kind_round_trips() {
    let (_dir, records) = records();
    round_trip(&records, Person::key("p1").unwrap(), person()).await;
    round_trip(&records, binding().key().unwrap(), binding()).await;
    round_trip(&records, grant().key().unwrap(), grant()).await;
    round_trip(&records, channel().key().unwrap(), channel()).await;
    round_trip(&records, token().key(), token()).await;
    round_trip(&records, audit().key("n1").unwrap(), audit()).await;
}

#[tokio::test]
async fn a_missing_record_is_none() {
    let (_dir, records) = records();
    let got = records.get::<Person>(&Person::key("nobody").unwrap()).await;
    assert!(got.unwrap().is_none());
}

#[tokio::test]
async fn reading_a_key_as_the_wrong_kind_is_an_error() {
    let (_dir, records) = records();
    let key = Person::key("p1").unwrap();
    let _ = records.create(key.clone(), person()).await.unwrap();

    let err = records.get::<RoleGrant>(&key).await.unwrap_err();
    assert!(matches!(err, RecordsError::WrongKind { .. }), "{err}");
}

#[tokio::test]
async fn an_update_replaces_the_value_and_advances_the_revision() {
    let (_dir, records) = records();
    let key = channel().key().unwrap();
    let Write::Committed(first) = records.create(key.clone(), channel()).await.unwrap() else {
        panic!("create conflicted");
    };

    let raised = ChannelConfig {
        ceiling: FleetRole::Operator,
        ..channel()
    };
    let Write::Committed(second) = records.update(&first, raised.clone()).await.unwrap() else {
        panic!("update conflicted");
    };
    assert_ne!(second.revision, first.revision);

    let read = records.get::<ChannelConfig>(&key).await.unwrap().unwrap();
    assert_eq!(read.value, raised);
    assert_eq!(read.revision, second.revision);
}

#[tokio::test]
async fn creating_an_existing_record_is_a_conflict_carrying_the_current_value() {
    let (_dir, records) = records();
    let key = Person::key("p1").unwrap();
    let _ = records.create(key.clone(), person()).await.unwrap();

    let other = Person {
        display_name: "Someone else".into(),
        email: None,
    };
    match records.create(key, other).await.unwrap() {
        Write::Conflict(Some(current)) => assert_eq!(current.value, person()),
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn concurrent_updates_from_one_read_give_one_commit_and_one_conflict() {
    let (_dir, records) = records();
    let key = grant().key().unwrap();
    let Write::Committed(read) = records.create(key.clone(), grant()).await.unwrap() else {
        panic!("create conflicted");
    };

    let admin = RoleGrant {
        role: FleetRole::Admin,
        ..grant()
    };
    let viewer = RoleGrant {
        role: FleetRole::Viewer,
        ..grant()
    };
    let (a, b) = tokio::join!(records.update(&read, admin), records.update(&read, viewer));
    let outcomes = [a.unwrap(), b.unwrap()];

    let committed: Vec<_> = outcomes
        .iter()
        .filter_map(|w| match w {
            Write::Committed(v) => Some(v.value.clone()),
            Write::Conflict(_) => None,
        })
        .collect();
    let conflicts: Vec<_> = outcomes
        .iter()
        .filter_map(|w| match w {
            Write::Conflict(current) => Some(current.clone()),
            Write::Committed(_) => None,
        })
        .collect();
    assert_eq!(committed.len(), 1, "{outcomes:?}");
    assert_eq!(conflicts.len(), 1, "{outcomes:?}");
    // The loser sees the winner's write, so it can retry on top of it.
    let current = conflicts[0].as_ref().expect("current record");
    assert_eq!(current.value, committed[0]);
}

#[tokio::test]
async fn a_delete_removes_the_record_and_a_stale_delete_conflicts() {
    let (_dir, records) = records();
    let key = grant().key().unwrap();
    let Write::Committed(first) = records.create(key.clone(), grant()).await.unwrap() else {
        panic!("create conflicted");
    };
    let raised = RoleGrant {
        role: FleetRole::Admin,
        ..grant()
    };
    let Write::Committed(second) = records.update(&first, raised).await.unwrap() else {
        panic!("update conflicted");
    };

    match records.delete(&first).await.unwrap() {
        Delete::Conflict(Some(current)) => assert_eq!(current.revision, second.revision),
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert!(matches!(
        records.delete(&second).await.unwrap(),
        Delete::Deleted
    ));
    assert!(records.get::<RoleGrant>(&key).await.unwrap().is_none());
}

#[tokio::test]
async fn list_returns_the_keys_of_one_kind_only() {
    let (_dir, records) = records();
    let _ = records
        .create(Person::key("p1").unwrap(), person())
        .await
        .unwrap();
    let _ = records
        .create(Person::key("p2").unwrap(), person())
        .await
        .unwrap();
    let _ = records
        .create(grant().key().unwrap(), grant())
        .await
        .unwrap();

    let mut keys = records.list::<Person>().await.unwrap();
    keys.sort();
    assert_eq!(
        keys,
        vec![Person::key("p1").unwrap(), Person::key("p2").unwrap()]
    );
}

#[tokio::test]
async fn appended_audit_entries_get_distinct_keys_in_the_audit_namespace() {
    let (_dir, records) = records();
    let a = records.append_audit(&audit()).await.unwrap();
    let b = records.append_audit(&audit()).await.unwrap();
    assert_ne!(a, b);
    assert_eq!(a.namespace, "fleet-audit");

    let stored = records.get::<AuditEntry>(&a).await.unwrap().unwrap();
    assert_eq!(stored.value, audit());
    assert_eq!(records.list::<AuditEntry>().await.unwrap().len(), 2);
}

#[test]
fn connect_rejects_a_malformed_url() {
    let err = Records::connect("not a url", "token", Identity::new("arield")).unwrap_err();
    assert!(matches!(err, RecordsError::Store(_)), "{err}");
}

#[test]
fn connect_accepts_a_gonzalod_url_without_contacting_it() {
    Records::connect("http://127.0.0.1:1", "token", Identity::new("arield")).unwrap();
}

#[tokio::test]
async fn a_channel_following_the_whole_fleet_round_trips() {
    let (_dir, records) = records();
    let fleet_wide = ChannelConfig {
        follows: Follows::Fleet,
        notify: NotifyPreset::All,
        ..channel()
    };
    round_trip(&records, fleet_wide.key().unwrap(), fleet_wide).await;
}

#[test]
fn a_channel_must_follow_at_least_one_workspace() {
    assert!(Follows::workspaces(Vec::<String>::new()).is_err());
}
