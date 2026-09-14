//! Golden tests for rendering an agent's state into chat messages (#13,
//! ADR 0007). Every expected message is written out by hand.

use ariel_core::chat::{Message, Severity, Url};
use ariel_core::prospero::types::{
    AgentStatus, EventKind, FleetEvent, OutputStream, WorkspaceHealth,
};
use ariel_core::render::{AgentView, render_agent, render_summary};

fn event(ts: &str, kind: EventKind) -> FleetEvent {
    FleetEvent {
        seq: 0,
        ts: ts.to_owned(),
        repo: "caliban".to_owned(),
        agent_id: "a1".to_owned(),
        kind,
    }
}

fn status(from: AgentStatus, to: AgentStatus) -> EventKind {
    EventKind::StatusChanged { from, to }
}

fn finished(outcome: &str) -> EventKind {
    EventKind::AgentFinished {
        outcome: outcome.to_owned(),
        cost_usd: 0.42,
        turns: 7,
    }
}

fn fix_tests() -> AgentView {
    AgentView::new("caliban", "a1").with_name("fix-tests")
}

fn fields(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn spawned_agent_renders_spawning_with_its_start_time() {
    let mut view = fix_tests();

    assert!(view.apply(&event("14:02", EventKind::AgentSpawned)));

    assert_eq!(
        render_agent(&view, None),
        Message {
            title: Some("caliban · fix-tests".to_owned()),
            body: "spawning".to_owned(),
            fields: fields(&[("started", "14:02")]),
            severity: Severity::Info,
            link: None,
            actions: vec![],
        }
    );
}

#[test]
fn status_change_updates_the_live_message_once() {
    let mut view = fix_tests();
    view.apply(&event("14:02", EventKind::AgentSpawned));

    assert!(view.apply(&event(
        "14:03",
        status(AgentStatus::Spawning, AgentStatus::Running)
    )));
    assert!(
        !view.apply(&event(
            "14:04",
            status(AgentStatus::Running, AgentStatus::Running)
        )),
        "a repeated status is not a change"
    );

    assert_eq!(render_agent(&view, None).body, "running");
}

#[test]
fn successful_finish_renders_outcome_cost_and_turns() {
    let mut view = fix_tests();
    view.apply(&event("14:02", EventKind::AgentSpawned));
    view.apply(&event(
        "14:03",
        status(AgentStatus::Spawning, AgentStatus::Running),
    ));
    view.apply(&event(
        "14:19",
        status(AgentStatus::Running, AgentStatus::Done),
    ));
    assert!(view.apply(&event("14:19", finished("success"))));

    assert!(view.is_terminal());
    assert_eq!(
        render_agent(&view, None),
        Message {
            title: Some("caliban · fix-tests".to_owned()),
            body: "done".to_owned(),
            fields: fields(&[
                ("started", "14:02"),
                ("ended", "14:19"),
                ("outcome", "success"),
                ("cost", "$0.42"),
                ("turns", "7"),
            ]),
            severity: Severity::Success,
            link: None,
            actions: vec![],
        }
    );
}

#[test]
fn failed_and_crashed_render_as_failures() {
    for (to, label) in [
        (AgentStatus::Failed, "failed"),
        (AgentStatus::Crashed, "crashed"),
    ] {
        let mut view = fix_tests();
        view.apply(&event("14:02", EventKind::AgentSpawned));
        view.apply(&event("14:06", status(AgentStatus::Running, to)));

        let message = render_agent(&view, None);
        assert_eq!(message.body, label);
        assert_eq!(message.severity, Severity::Failure, "{label}");
        assert_eq!(
            message.fields,
            fields(&[("started", "14:02"), ("ended", "14:06")])
        );
        assert!(view.is_failure(), "{label}");
    }
}

#[test]
fn killed_gone_and_unsuccessful_outcomes_render_as_warnings() {
    let mut killed = fix_tests();
    killed.apply(&event(
        "14:05",
        status(AgentStatus::Running, AgentStatus::Killed),
    ));

    let mut gone = fix_tests();
    gone.apply(&event("14:02", EventKind::AgentSpawned));
    gone.apply(&event("14:05", EventKind::AgentGone));

    let mut max_turns = fix_tests();
    max_turns.apply(&event("14:09", finished("max_turns")));

    for (view, body) in [
        (&killed, "killed"),
        (&gone, "gone"),
        (&max_turns, "finished"),
    ] {
        let message = render_agent(view, None);
        assert_eq!(message.body, body);
        assert_eq!(message.severity, Severity::Warning, "{body}");
        assert!(view.is_terminal(), "{body}");
        assert!(!view.is_failure(), "{body}");
    }
    assert_eq!(
        render_agent(&gone, None).fields,
        fields(&[("started", "14:02"), ("ended", "14:05")])
    );
}

#[test]
fn events_that_do_not_notify_leave_the_view_unchanged() {
    let noise = [
        EventKind::AgentDiscovered,
        EventKind::AgentInit {
            model: "claude-opus-5".to_owned(),
            tools: vec!["Read".to_owned()],
            session_id: "s-1".to_owned(),
        },
        EventKind::Output {
            stream: OutputStream::Stdout,
            chunk: "hi".to_owned(),
        },
        EventKind::ToolStarted {
            id: "t1".to_owned(),
            name: "Read".to_owned(),
            input: serde_json::json!({ "path": "src/lib.rs" }),
        },
        EventKind::ToolFinished {
            id: "t1".to_owned(),
            name: String::new(),
            ok: true,
        },
        EventKind::StorePersistFailed {
            lost_seq: 5,
            detail: "disk full".to_owned(),
        },
        EventKind::RepoHealth {
            state: WorkspaceHealth::Healthy,
        },
        EventKind::Unknown,
    ];

    let mut view = fix_tests();
    view.apply(&event("14:02", EventKind::AgentSpawned));
    let before = view.clone();

    for kind in noise {
        let label = format!("{kind:?}");
        assert!(!view.apply(&event("14:03", kind)), "{label}");
    }
    assert_eq!(view, before);
}

#[test]
fn dashboard_link_is_included_only_when_configured() {
    let view = fix_tests();
    let dashboard = Url::parse("https://prospero.example/").unwrap();

    assert_eq!(render_agent(&view, Some(&dashboard)).link, Some(dashboard));
    assert_eq!(render_agent(&view, None).link, None);
}

#[test]
fn title_falls_back_to_the_agent_id() {
    let view = AgentView::new("gonzalo", "a7");

    assert_eq!(
        render_agent(&view, None).title.as_deref(),
        Some("gonzalo · a7")
    );
}

fn member(id: &str, events: &[EventKind]) -> AgentView {
    let mut view = AgentView::new("caliban", id);
    for kind in events {
        view.apply(&event("14:02", kind.clone()));
    }
    view
}

#[test]
fn summary_counts_members_by_state() {
    let mut members = Vec::new();
    for i in 0..8 {
        members.push(member(
            &format!("d{i}"),
            &[
                status(AgentStatus::Running, AgentStatus::Done),
                finished("success"),
            ],
        ));
    }
    for i in 0..3 {
        members.push(member(
            &format!("r{i}"),
            &[status(AgentStatus::Spawning, AgentStatus::Running)],
        ));
    }
    members.push(member(
        "c0",
        &[status(AgentStatus::Running, AgentStatus::Crashed)],
    ));

    assert_eq!(
        render_summary("caliban", &members, None),
        Message {
            title: Some("caliban · 12 agents".to_owned()),
            body: "8 done · 3 running · 1 failed · 0 stopped".to_owned(),
            fields: vec![],
            severity: Severity::Info,
            link: None,
            actions: vec![],
        }
    );
}

#[test]
fn settled_summary_severity_reflects_how_members_ended() {
    let done = || {
        member(
            "d",
            &[
                status(AgentStatus::Running, AgentStatus::Done),
                finished("success"),
            ],
        )
    };
    let crashed = member("c", &[status(AgentStatus::Running, AgentStatus::Crashed)]);
    let killed = member("k", &[status(AgentStatus::Running, AgentStatus::Killed)]);
    let dashboard = Url::parse("https://prospero.example/").unwrap();

    let all_done = render_summary("caliban", &[done(), done()], Some(&dashboard));
    assert_eq!(all_done.body, "2 done · 0 running · 0 failed · 0 stopped");
    assert_eq!(all_done.severity, Severity::Success);
    assert_eq!(all_done.link, Some(dashboard));

    let with_crash = render_summary("caliban", &[done(), crashed], None);
    assert_eq!(with_crash.body, "1 done · 0 running · 1 failed · 0 stopped");
    assert_eq!(with_crash.severity, Severity::Warning);

    let with_kill = render_summary("caliban", &[done(), killed], None);
    assert_eq!(with_kill.body, "1 done · 0 running · 0 failed · 1 stopped");
    assert_eq!(with_kill.severity, Severity::Warning);
}
