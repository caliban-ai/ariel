//! Reading and changing a channel's configuration (#18, ADR 0009).

use std::sync::Arc;

use ariel_core::channels::{self, Applied, ChannelKey, ChannelPatch, ChannelsError};
use ariel_core::records::Records;
use ariel_core::records::gonzalo::{
    AuditEntry, FleetActor, FleetRole, Follows, FsStore, Identity, NotifyPreset,
};
use tempfile::TempDir;

fn records() -> (TempDir, Records) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FsStore::new(dir.path()));
    (dir, Records::new(store, Identity::new("ariel-cli-test")))
}

fn key() -> ChannelKey {
    ChannelKey {
        provider: "discord".into(),
        tenant: "guild-1".into(),
        channel: "ops".into(),
    }
}

fn actor() -> FleetActor {
    FleetActor::Service("ariel-cli".into())
}

/// A patch that creates a channel following the whole fleet.
fn create_fleet() -> ChannelPatch {
    ChannelPatch {
        follows: Some(Follows::Fleet),
        ..ChannelPatch::default()
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
async fn set_then_show_round_trips() {
    let (_dir, records) = records();
    let patch = ChannelPatch {
        follows: Some(Follows::workspaces(["caliban"]).unwrap()),
        notify: Some(NotifyPreset::Failures),
        ceiling: Some(FleetRole::Operator),
    };

    let applied = channels::set(&records, &key(), patch, &actor())
        .await
        .unwrap();
    let Applied::Created(created) = applied else {
        panic!("expected a new channel, got {applied:?}");
    };
    assert_eq!(created.ceiling, FleetRole::Operator);

    let shown = channels::show(&records, &key())
        .await
        .unwrap()
        .expect("the channel is configured");
    assert_eq!(shown.provider, "discord");
    assert_eq!(shown.tenant, "guild-1");
    assert_eq!(shown.channel, "ops");
    assert_eq!(shown.follows, Follows::workspaces(["caliban"]).unwrap());
    assert_eq!(shown.notify, NotifyPreset::Failures);
    assert_eq!(shown.ceiling, FleetRole::Operator);
}

#[tokio::test]
async fn an_unconfigured_channel_shows_nothing() {
    let (_dir, records) = records();
    assert!(channels::show(&records, &key()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_new_channel_must_say_what_it_follows() {
    let (_dir, records) = records();
    let patch = ChannelPatch {
        ceiling: Some(FleetRole::Admin),
        ..ChannelPatch::default()
    };

    let error = channels::set(&records, &key(), patch, &actor())
        .await
        .unwrap_err();
    assert!(
        matches!(error, ChannelsError::FollowsRequired),
        "a channel with no follows would hear nothing: {error}"
    );
    assert!(channels::show(&records, &key()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_patch_changes_only_what_it_names() {
    let (_dir, records) = records();
    channels::set(&records, &key(), create_fleet(), &actor())
        .await
        .unwrap();

    let raise = ChannelPatch {
        ceiling: Some(FleetRole::Admin),
        ..ChannelPatch::default()
    };
    let applied = channels::set(&records, &key(), raise, &actor())
        .await
        .unwrap();
    let Applied::Updated { before, after } = applied else {
        panic!("expected an update, got {applied:?}");
    };
    assert_eq!(before.ceiling, FleetRole::Viewer, "the default ceiling");
    assert_eq!(after.ceiling, FleetRole::Admin);
    // Untouched fields survive.
    assert_eq!(after.follows, Follows::Fleet);
    assert_eq!(after.notify, NotifyPreset::All);
}

#[tokio::test]
async fn a_patch_that_changes_nothing_is_not_written_again() {
    let (_dir, records) = records();
    channels::set(&records, &key(), create_fleet(), &actor())
        .await
        .unwrap();
    let before = audit_entries(&records).await.len();

    let applied = channels::set(&records, &key(), create_fleet(), &actor())
        .await
        .unwrap();
    assert!(
        matches!(applied, Applied::Unchanged(_)),
        "expected no change, got {applied:?}"
    );
    assert_eq!(
        audit_entries(&records).await.len(),
        before,
        "an unchanged channel is not audited again"
    );
}

#[tokio::test]
async fn every_change_is_audited() {
    let (_dir, records) = records();
    channels::set(&records, &key(), create_fleet(), &actor())
        .await
        .unwrap();
    let raise = ChannelPatch {
        ceiling: Some(FleetRole::Admin),
        ..ChannelPatch::default()
    };
    channels::set(&records, &key(), raise, &actor())
        .await
        .unwrap();

    let entries = audit_entries(&records).await;
    assert_eq!(entries.len(), 2, "one entry per change: {entries:?}");
    assert!(
        entries
            .iter()
            .all(|entry| entry.target == "fleet/channels/discord:guild-1:ops"),
        "entries name the channel they changed: {entries:?}"
    );
    assert!(
        entries.iter().any(|entry| entry.action == "channel.create"),
        "{entries:?}"
    );
    let update = entries
        .iter()
        .find(|entry| entry.action == "channel.update")
        .expect("the ceiling change is audited");
    assert_eq!(update.actor, actor());
}

#[tokio::test]
async fn a_concurrent_edit_is_a_conflict_showing_what_is_stored() {
    let (_dir, records) = records();
    channels::set(&records, &key(), create_fleet(), &actor())
        .await
        .unwrap();

    // Both operators read the same revision, then write.
    let who = actor();
    let mine = channels::read(&records, &key()).await.unwrap().unwrap();
    let theirs = mine.clone();
    let (first, second) = tokio::join!(
        channels::apply(
            &records,
            mine,
            ChannelPatch {
                ceiling: Some(FleetRole::Admin),
                ..ChannelPatch::default()
            },
            &who,
        ),
        channels::apply(
            &records,
            theirs,
            ChannelPatch {
                ceiling: Some(FleetRole::Operator),
                ..ChannelPatch::default()
            },
            &who,
        )
    );

    let outcomes = [first.unwrap(), second.unwrap()];
    let updated = outcomes
        .iter()
        .filter(|applied| matches!(applied, Applied::Updated { .. }))
        .count();
    let conflicts: Vec<_> = outcomes
        .iter()
        .filter_map(|applied| match applied {
            Applied::Conflict(current) => Some(current.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(updated, 1, "{outcomes:?}");
    assert_eq!(conflicts.len(), 1, "{outcomes:?}");

    // The loser is shown what is actually stored, not told it succeeded.
    let stored = channels::show(&records, &key()).await.unwrap().unwrap();
    assert_eq!(
        conflicts[0].as_ref().expect("the current record").ceiling,
        stored.ceiling
    );
}
