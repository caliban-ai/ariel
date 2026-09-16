//! The per-channel notifier (#19, ADR 0007 and ADR 0009).
//!
//! Every test runs against a provider it can script, with tokio time paused, so
//! the counts and the gaps between sends are exact.

mod support;

use std::sync::Arc;
use std::time::Duration;

use ariel_core::chat::console::Recorded;
use ariel_core::chat::{ChannelRef, Destination, ProviderError, Severity};
use ariel_core::notify::{Notifier, NotifyConfig, Route};
use ariel_core::prospero::types::{AgentStatus, FleetEvent};
use ariel_core::records::gonzalo::{ChannelConfig, FleetRole, Follows, NotifyPreset};
use support::{ScriptedProvider, crashed, finished, spawned, status};

fn channel() -> ChannelRef {
    ChannelRef::new("console", "tenant-1", "ops")
}

fn route(follows: Follows, notify: NotifyPreset) -> Route {
    Route::from_config(&ChannelConfig {
        provider: "console".into(),
        tenant: "tenant-1".into(),
        channel: "ops".into(),
        follows,
        notify,
        ceiling: FleetRole::Viewer,
    })
}

fn follows_caliban() -> Follows {
    Follows::workspaces(["caliban"]).unwrap()
}

/// Start a notifier for `route` with the default timings.
fn start(provider: Arc<ScriptedProvider>, route: Route) -> tokio::sync::mpsc::Sender<FleetEvent> {
    Notifier::new(provider, route, NotifyConfig::default()).spawn(64)
}

/// Let every ready task run without advancing the clock.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// Advance paused time, letting tasks run at each step.
async fn advance(by: Duration) {
    settle().await;
    tokio::time::advance(by).await;
    settle().await;
}

/// Advance until the notifier stops sending, so an assertion sees its final
/// state rather than a moment mid-pause. Bounded, so a stuck notifier fails the
/// test rather than hanging it.
async fn quiesce(provider: &ScriptedProvider) {
    // A second at a time, so every pace, hold and pause in between elapses;
    // long enough to outlast the longest wait these tests script.
    let _ = provider;
    for _ in 0..30 {
        advance(Duration::from_secs(1)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_lone_agent_gets_one_post_edited_in_place() {
    let provider = Arc::new(ScriptedProvider::new());
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
    );

    events.send(spawned("caliban", "a1")).await.unwrap();
    // The first post waits out the hold, so a lone agent is told apart from a
    // burst.
    advance(Duration::from_millis(1_900)).await;
    assert_eq!(provider.log().len(), 0, "posted before the hold elapsed");
    advance(Duration::from_millis(200)).await;
    assert!(matches!(
        provider.log().as_slice(),
        [Recorded::Posted { .. }]
    ));

    events
        .send(status("caliban", "a1", AgentStatus::Running))
        .await
        .unwrap();
    advance(Duration::from_secs(3)).await;
    events
        .send(finished("caliban", "a1", "success", 0.42, 7))
        .await
        .unwrap();
    advance(Duration::from_secs(3)).await;

    let log = provider.log();
    assert_eq!(
        log.iter()
            .filter(|r| matches!(r, Recorded::Posted { .. }))
            .count(),
        1,
        "one live message only: {log:?}"
    );
    let Some(Recorded::Edited { message, .. }) = log.last() else {
        panic!("expected the last send to be an edit: {log:?}");
    };
    assert_eq!(message.severity, Severity::Success);
    let fields: Vec<&str> = message.fields.iter().map(|(k, _)| k.as_str()).collect();
    assert!(
        fields.contains(&"outcome") && fields.contains(&"cost") && fields.contains(&"turns"),
        "final message shows the outcome: {message:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn edits_to_one_message_are_spaced_apart() {
    let provider = Arc::new(ScriptedProvider::new());
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
    );

    events.send(spawned("caliban", "a1")).await.unwrap();
    advance(Duration::from_secs(3)).await;

    // Twenty changes inside three seconds collapse into at most two edits.
    for (i, s) in [AgentStatus::Running, AgentStatus::Idle]
        .into_iter()
        .cycle()
        .take(20)
        .enumerate()
    {
        events.send(status("caliban", "a1", s)).await.unwrap();
        if i % 7 == 6 {
            advance(Duration::from_millis(150)).await;
        }
    }
    advance(Duration::from_secs(3)).await;

    let edits = provider
        .log()
        .iter()
        .filter(|r| matches!(r, Recorded::Edited { .. }))
        .count();
    assert!(
        (1..=2).contains(&edits),
        "20 changes over 3s should collapse to one or two edits, got {edits}"
    );
}

#[tokio::test(start_paused = true)]
async fn events_from_unfollowed_workspaces_are_ignored() {
    let provider = Arc::new(ScriptedProvider::new());
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
    );

    events.send(spawned("prospero", "b1")).await.unwrap();
    events
        .send(finished("prospero", "b1", "success", 0.1, 1))
        .await
        .unwrap();
    advance(Duration::from_secs(5)).await;

    assert!(provider.log().is_empty(), "{:?}", provider.log());
}

#[tokio::test(start_paused = true)]
async fn a_fleet_channel_follows_every_workspace() {
    let provider = Arc::new(ScriptedProvider::new());
    let events = start(provider.clone(), route(Follows::Fleet, NotifyPreset::All));

    events.send(spawned("prospero", "b1")).await.unwrap();
    advance(Duration::from_secs(3)).await;

    assert_eq!(provider.log().len(), 1, "{:?}", provider.log());
}

#[tokio::test(start_paused = true)]
async fn a_burst_shares_one_summary_and_failures_still_post() {
    let provider = Arc::new(ScriptedProvider::new());
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
    );

    for i in 0..6 {
        events
            .send(spawned("caliban", &format!("a{i}")))
            .await
            .unwrap();
    }
    advance(Duration::from_secs(3)).await;

    let posts = provider
        .log()
        .iter()
        .filter(|r| matches!(r, Recorded::Posted { .. }))
        .count();
    assert_eq!(
        posts,
        1,
        "six spawns share one summary: {:?}",
        provider.log()
    );

    // A crash gets its own post, so no failure hides inside a count.
    events.send(crashed("caliban", "a3")).await.unwrap();
    quiesce(&provider).await;

    let log = provider.log();
    let posts: Vec<_> = log
        .iter()
        .filter_map(|r| match r {
            Recorded::Posted { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(posts.len(), 2, "summary plus one failure post: {log:?}");
    assert_eq!(posts[1].severity, Severity::Failure);

    for i in [0, 1, 2, 4, 5] {
        events
            .send(finished("caliban", &format!("a{i}"), "success", 0.1, 2))
            .await
            .unwrap();
    }
    quiesce(&provider).await;

    let log = provider.log();
    let Some(Recorded::Edited { message, .. }) =
        log.iter().rfind(|r| matches!(r, Recorded::Edited { .. }))
    else {
        panic!("expected the summary to be edited: {log:?}");
    };
    assert!(
        message.body.contains("5 done") && message.body.contains("1 failed"),
        "summary counts its members: {message:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_terminal_preset_posts_once_at_the_end() {
    let provider = Arc::new(ScriptedProvider::new());
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::Terminal),
    );

    events.send(spawned("caliban", "a1")).await.unwrap();
    events
        .send(status("caliban", "a1", AgentStatus::Running))
        .await
        .unwrap();
    advance(Duration::from_secs(5)).await;
    assert!(
        provider.log().is_empty(),
        "no live message before the end: {:?}",
        provider.log()
    );

    events
        .send(finished("caliban", "a1", "success", 0.2, 3))
        .await
        .unwrap();
    advance(Duration::from_secs(5)).await;
    assert!(
        matches!(provider.log().as_slice(), [Recorded::Posted { .. }]),
        "{:?}",
        provider.log()
    );
}

#[tokio::test(start_paused = true)]
async fn the_failures_preset_posts_only_failures() {
    let provider = Arc::new(ScriptedProvider::new());
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::Failures),
    );

    events.send(spawned("caliban", "ok")).await.unwrap();
    events
        .send(finished("caliban", "ok", "success", 0.2, 3))
        .await
        .unwrap();
    events.send(spawned("caliban", "bad")).await.unwrap();
    events.send(crashed("caliban", "bad")).await.unwrap();
    advance(Duration::from_secs(5)).await;

    let log = provider.log();
    assert_eq!(log.len(), 1, "only the crash notifies: {log:?}");
    let Some(Recorded::Posted { message, .. }) = log.first() else {
        panic!("expected a post: {log:?}");
    };
    assert_eq!(message.severity, Severity::Failure);
}

#[tokio::test(start_paused = true)]
async fn a_deleted_message_is_replaced_by_a_new_post() {
    let provider = Arc::new(ScriptedProvider::new());
    provider.fail_next_edit(ProviderError::NotFound);
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
    );

    events.send(spawned("caliban", "a1")).await.unwrap();
    advance(Duration::from_secs(3)).await;
    events
        .send(finished("caliban", "a1", "success", 0.2, 3))
        .await
        .unwrap();
    advance(Duration::from_secs(5)).await;

    let posts = provider
        .log()
        .iter()
        .filter(|r| matches!(r, Recorded::Posted { .. }))
        .count();
    assert_eq!(
        posts,
        2,
        "the failed edit is followed by one new post: {:?}",
        provider.log()
    );
}

#[tokio::test(start_paused = true)]
async fn losing_access_stops_sending_until_the_recheck() {
    let provider = Arc::new(ScriptedProvider::new());
    provider.fail_next_post(ProviderError::Forbidden);
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
    );

    events.send(spawned("caliban", "a1")).await.unwrap();
    advance(Duration::from_secs(3)).await;
    let attempts = provider.attempts();

    // Nothing is retried while the channel is marked unhealthy.
    events
        .send(status("caliban", "a1", AgentStatus::Running))
        .await
        .unwrap();
    advance(Duration::from_secs(60)).await;
    assert_eq!(
        provider.attempts(),
        attempts,
        "kept sending after Forbidden"
    );

    advance(Duration::from_secs(300)).await;
    assert!(
        provider.attempts() > attempts,
        "never retried after the re-check window"
    );
}

#[tokio::test(start_paused = true)]
async fn a_rate_limit_pauses_the_channel_then_sends_the_terminal_state() {
    let provider = Arc::new(ScriptedProvider::new());
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
    );

    events.send(spawned("caliban", "a1")).await.unwrap();
    advance(Duration::from_secs(3)).await;
    assert_eq!(provider.log().len(), 1);

    provider.fail_next_edit(ProviderError::RateLimited {
        retry_after: Some(Duration::from_secs(5)),
    });
    for _ in 0..10 {
        events
            .send(status("caliban", "a1", AgentStatus::Running))
            .await
            .unwrap();
        events
            .send(status("caliban", "a1", AgentStatus::Idle))
            .await
            .unwrap();
    }
    events
        .send(finished("caliban", "a1", "success", 0.5, 9))
        .await
        .unwrap();
    advance(Duration::from_secs(1)).await;
    let during_pause = provider.attempts();

    quiesce(&provider).await;
    let log = provider.log();
    let Some(Recorded::Edited { message, .. }) = log.last() else {
        panic!("expected an edit after the pause: {log:?}");
    };
    assert_eq!(
        message.severity,
        Severity::Success,
        "the state sent after the pause is the latest one: {message:?}"
    );
    assert!(
        provider.attempts() > during_pause,
        "sending never resumed after the pause"
    );
}

#[tokio::test(start_paused = true)]
async fn a_provider_without_edit_posts_once_and_posts_the_end() {
    let provider = Arc::new(ScriptedProvider::without_edit());
    let events = start(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
    );

    events.send(spawned("caliban", "a1")).await.unwrap();
    advance(Duration::from_secs(3)).await;
    events
        .send(status("caliban", "a1", AgentStatus::Running))
        .await
        .unwrap();
    advance(Duration::from_secs(3)).await;
    events
        .send(finished("caliban", "a1", "success", 0.2, 3))
        .await
        .unwrap();
    advance(Duration::from_secs(3)).await;

    let log = provider.log();
    assert!(
        log.iter().all(|r| matches!(r, Recorded::Posted { .. })),
        "a provider without edit never edits: {log:?}"
    );
    assert_eq!(
        log.len(),
        2,
        "one post at spawn and one at the end: {log:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_the_sender_stops_the_notifier() {
    let provider = Arc::new(ScriptedProvider::new());
    let notifier = Notifier::new(
        provider.clone(),
        route(follows_caliban(), NotifyPreset::All),
        NotifyConfig::default(),
    );
    let events = notifier.spawn(8);
    drop(events);
    advance(Duration::from_secs(1)).await;
    assert!(provider.log().is_empty());
}

#[test]
fn a_route_addresses_the_channel_its_record_names() {
    let route = route(follows_caliban(), NotifyPreset::All);
    assert_eq!(route.channel(), &channel());
    assert_eq!(
        route.destination(),
        &Destination::Channel(ChannelRef::new("console", "tenant-1", "ops"))
    );
    assert!(route.follows_workspace("caliban"));
    assert!(!route.follows_workspace("prospero"));
}
