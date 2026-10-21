//! The session's subagents through the live port: the driver lists the
//! host's children, publishes the list again when one starts, settles, or
//! goes, and passes a stop request to the host.

use bake_coding_agent::agent_session::SessionSelection;
use bake_coding_agent::jobs::{JobLimits, JobRegistry};
use bake_coding_agent::model_registry::ModelRuntime;
use bake_coding_agent::owned_work::WorkGate;
use bake_coding_agent::subagents::models::ChildRoute;
use bake_coding_agent::subagents::{HostOptions, Recovered, RecoveredChild, SubagentHost};
use bake_tui_view::agents::{Agent, AgentState, Ended};

use super::*;

fn agents(updates: &[RuntimeUpdate]) -> Option<&[Agent]> {
    updates.iter().rev().find_map(|update| match update {
        RuntimeUpdate::Agents(list) => Some(list.as_slice()),
        _ => None,
    })
}

#[test]
fn the_hosts_children_reach_the_view_as_they_start_and_go() {
    let (runtime, faux, mut live) = setup(Vec::new(), Vec::new(), Some("test-key"));
    let model = live.session.model().expect("model");
    let work = WorkGate::default();
    let host = SubagentHost::new(HostOptions {
        parent_id: "parent".into(),
        parent_file: None,
        jobs: JobRegistry::new(work.clone(), JobLimits::default()),
        work,
        selection: SessionSelection::default(),
        registry: ModelRuntime::new(ModelRegistry::new(
            ModelConfig::from_value(json!({}), "models.json"),
            AuthStorage::empty(),
        )),
        settings: Settings::default(),
        // A child whose session never finishes building stays starting.
        builder: Arc::new(|_| Box::pin(std::future::pending())),
        recovered: Recovered {
            children: vec![RecoveredChild {
                id: "gone".into(),
                label: "old survey".into(),
                file: None,
                provider: "faux".into(),
                model: "faux-1".into(),
            }],
            last_run: 0,
        },
    });
    live.session.set_subagents(host.clone());
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    let h = Harness {
        runtime,
        faux,
        port: Some(port),
        updates,
    };

    // A child the session's file records, whose own file is gone.
    let first = h.until(|update| matches!(update, RuntimeUpdate::Agents(_)));
    assert_eq!(
        agents(&first),
        Some(
            &[Agent {
                id: "gone".into(),
                name: "old survey".into(),
                state: AgentState::Unavailable,
                route: Some("faux/faux-1".into()),
                detail: None,
                file: None,
            }][..]
        )
    );

    // A child the model starts joins the list while its session is built.
    let start = h.runtime.spawn({
        let host = host.clone();
        async move {
            host.start(
                "count",
                "Count the files.",
                ChildRoute {
                    model,
                    thinking_level: None,
                },
                false,
            )
            .await
        }
    });
    let started =
        h.until(|update| matches!(update, RuntimeUpdate::Agents(list) if list.len() == 2));
    let child = agents(&started).expect("agents")[1].clone();
    assert_eq!(
        (child.name.as_str(), child.state, child.route.as_deref()),
        ("count", AgentState::Starting, Some("faux/faux-1"))
    );

    // A stop for it, which has no run to stop, or for an id the host never
    // had, reaches the host without ending the driver; the start, dropped,
    // takes its child away.
    h.port().request(Request::StopAgent(child.id.clone()));
    h.port().request(Request::StopAgent("unknown".into()));
    start.abort();
    let gone = h.until(|update| matches!(update, RuntimeUpdate::Agents(list) if list.len() == 1));
    assert_eq!(agents(&gone).expect("agents")[0].id, "gone");
    h.stop();
}

#[test]
fn a_stop_from_the_list_ends_a_working_childs_run_as_stopped() {
    let runtime = test_runtime();
    let faux = faux(Some("test-key"));
    let manager = SessionManager::in_memory(Some("/tmp"), NewSessionOptions::default(), Vec::new())
        .expect("session");
    let mut live = live_session(&runtime, &faux, manager, Vec::new(), Vec::new());
    let model = live.session.model().expect("model");
    let work = WorkGate::default();
    let host = SubagentHost::new(HostOptions {
        parent_id: "parent".into(),
        parent_file: None,
        jobs: JobRegistry::new(work.clone(), JobLimits::default()),
        work,
        selection: SessionSelection::default(),
        registry: Arc::clone(&faux.registry).into(),
        settings: Settings::default(),
        builder: Arc::new({
            let faux = faux.clone();
            move |_| {
                let faux = faux.clone();
                Box::pin(async move {
                    let store = SessionManager::in_memory(
                        Some("/tmp"),
                        NewSessionOptions::default(),
                        Vec::new(),
                    )
                    .map_err(|error| error.to_string())?;
                    faux_session(&faux, store, Vec::new()).await
                })
            }
        }),
        recovered: Recovered::default(),
    });
    live.session.set_subagents(host.clone());
    // The child's response waits until its run is stopped.
    let gate = Arc::new(Notify::new());
    faux.provider.set_responses(vec![gated(&gate, "Counted.")]);
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    let h = Harness {
        runtime,
        faux: faux.provider,
        port: Some(port),
        updates,
    };

    let start = h.runtime.spawn({
        let host = host.clone();
        async move {
            host.start(
                "count",
                "Count the files.",
                ChildRoute {
                    model,
                    thinking_level: None,
                },
                false,
            )
            .await
        }
    });
    let state = |wanted: AgentState| {
        move |update: &RuntimeUpdate| {
            matches!(update, RuntimeUpdate::Agents(list)
                if list.first().is_some_and(|agent| agent.state == wanted))
        }
    };
    let working = h.until(state(AgentState::Working));
    let id = agents(&working).expect("agents")[0].id.clone();
    // Its request is in flight, so the stop has a run to end.
    h.wait_for_requests(1);
    h.port().request(Request::StopAgent(id));
    h.until(state(AgentState::Idle(Some(Ended::Stopped))));
    assert!(h.runtime.block_on(start).expect("start").is_ok());
    h.stop();
}
