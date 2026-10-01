// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use reflex_sim::{
    autoscaler::{
        engine::{invariant, Choice, Control, Group, NodePhase, Phase, DRAIN_MS, HORIZON_MS},
        judge::{Evaluation, Evaluator, Evidence, Inference},
        presets::{cycle_replicas, CYCLE_MS},
        Command, Scenario, Session, TimelineEvent,
    },
    playground::inference::JevSettings,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Notify;

const WEB: usize = 0;
const API: usize = 1;
const BATCH: usize = 2;

fn settings() -> JevSettings {
    JevSettings {
        dispatch_interval: Duration::ZERO,
        ..Default::default()
    }
}
fn surge(workload: usize, enabled: bool) -> Command {
    Command::Control {
        control: Control::ReplicaSurge { workload, enabled },
    }
}
fn scenario(scenario: Scenario) -> Command {
    Command::Scenario { scenario }
}
/// Run the session clock, letting evaluation tasks make progress between ticks.
async fn run(s: &mut Session, ms: u64, tick: u64) {
    let until = s.data().at_ms + ms;
    while s.data().at_ms < until && !s.view().paused {
        s.tick(tick.min(until - s.data().at_ms)).await.unwrap();
        tokio::task::yield_now().await;
    }
}
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
/// Nodes, pods and revision: everything a recommendation could have changed.
fn cluster(s: &Session) -> serde_json::Value {
    let d = s.data();
    serde_json::json!({"revision":d.revision,"nodes":d.nodes,"pods":d.pods,"phase":s.phase()})
}

type Answer = fn(&Evidence) -> Inference;
/// A provider that answers only when released.
struct Slow {
    release: Notify,
    started: AtomicUsize,
    finished: AtomicUsize,
    answer: Answer,
}
impl Slow {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            release: Notify::new(),
            started: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            answer,
        })
    }
}
impl Evaluator for Slow {
    fn evaluate(&self, evidence: Evidence) -> Evaluation<'_> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.release.notified().await;
            self.finished.fetch_add(1, Ordering::SeqCst);
            (self.answer)(&evidence)
        })
    }
}
fn large(count: u8) -> Choice {
    Choice::ScaleUp {
        group: Group::GeneralLarge,
        count,
    }
}
fn scale_up(_: &Evidence) -> Inference {
    Inference::chose(large(1), Some(0.99))
}
/// Answers at once and records what it was shown.
struct Scripted {
    answer: Answer,
    seen: Mutex<Vec<Evidence>>,
}
impl Scripted {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            seen: Mutex::new(vec![]),
        })
    }
}
impl Evaluator for Scripted {
    fn evaluate(&self, evidence: Evidence) -> Evaluation<'_> {
        let inference = (self.answer)(&evidence);
        self.seen.lock().unwrap().push(evidence);
        Box::pin(async move { inference })
    }
}
/// A simple reactive autoscaler: add nodes for pending pods, remove empty nodes.
fn reactive(e: &Evidence) -> Inference {
    let offered = |c: &Choice| e.legal_actions.contains(c);
    let choice = if !e.pending_pods.is_empty() {
        [Group::GeneralLarge, Group::GeneralSmall, Group::MemoryHeavy]
            .into_iter()
            .flat_map(|group| [2, 1].map(|count| Choice::ScaleUp { group, count }))
            .find(|c| e.provisioning.is_empty() && offered(c))
    } else {
        e.nodes.iter().find(|n| n.pods.is_empty()).and_then(|n| {
            e.legal_actions
                .iter()
                .copied()
                .find(|c| c.label() == format!("remove:{}", n.name))
        })
    };
    Inference::chose(choice.unwrap_or(Choice::NoChange), Some(0.8))
}

#[tokio::test]
async fn a_slow_evaluation_does_not_block_the_clock_and_goes_stale() {
    let slow = Slow::new(scale_up);
    let mut s = Session::new(42, Some(slow.clone()), settings()).unwrap();
    assert!(s.view().paused && s.view().available);
    s.command(Command::Play).await.unwrap();
    run(&mut s, 10_000, 50).await;
    let view = s.view();
    assert_eq!(view.at_ms, 10_000);
    assert_eq!(view.history.len(), 10);
    assert_eq!((view.calls, slow.started.load(Ordering::SeqCst)), (1, 1));
    let pending = view.pending.unwrap();
    assert!(!pending.response_ready);
    assert_eq!(pending.observed_at_ms, 50);
    assert!(view.decisions.is_empty());

    // Load keeps changing while Jev is deciding; the revision moves on.
    s.command(surge(WEB, true)).await.unwrap();
    assert_eq!(s.view().pending_pods, 8);
    slow.release.notify_one();
    settle().await;
    assert!(s.view().pending.unwrap().response_ready);

    // A finished response waits while the run is paused.
    s.command(Command::Pause).await.unwrap();
    s.tick(50).await.unwrap();
    assert!(s.view().decisions.is_empty());
    let before = cluster(&s);
    s.command(Command::Play).await.unwrap();
    s.tick(50).await.unwrap();
    let view = s.view();
    let decision = &view.decisions[0];
    assert_eq!(decision.choice, Some(large(1)));
    assert_eq!(decision.status, "rejected");
    assert_eq!(decision.code.as_deref(), Some("fresh"));
    assert_eq!((decision.observed_at_ms, decision.at_ms), (50, 10_050));
    assert_eq!((decision.from, decision.to), (Phase::Stable, Phase::Stable));
    assert_eq!(
        before,
        cluster(&s),
        "a stale recommendation changes nothing"
    );
    assert_eq!(s.data().count(None, NodePhase::Provisioning), 0);
    // The next evaluation starts from the current cluster.
    assert_eq!(view.calls, 2);
    assert_eq!(view.pending.unwrap().observed_at_ms, 10_050);
    settle().await;
    assert_eq!(slow.started.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn evaluation_errors_and_unoffered_actions_leave_the_cluster_unchanged() {
    fn timeout(_: &Evidence) -> Inference {
        Inference::failed("timeout", "inference deadline exceeded")
    }
    fn unoffered(_: &Evidence) -> Inference {
        let node = Choice::Remove {
            group: Group::GeneralLarge,
            node: 99,
        };
        Inference::chose(node, Some(1.))
    }
    fn silent(_: &Evidence) -> Inference {
        let mut inference = Inference::chose(Choice::NoChange, None);
        inference.choice = None;
        inference
    }
    // Removing a node while pods are pending is unsafe however confident Jev is.
    fn shrink(e: &Evidence) -> Inference {
        let removal = e
            .legal_actions
            .iter()
            .find(|c| matches!(c, Choice::Remove { .. }));
        Inference::chose(*removal.unwrap(), Some(1.))
    }
    for (answer, status, code) in [
        (timeout as Answer, "evaluation_error", "timeout"),
        (shrink, "rejected", "scale_down_cooldown"),
        (unoffered, "rejected", "not_offered"),
        (silent, "evaluation_error", "missing_action"),
    ] {
        let mut s = Session::new(42, Some(Scripted::new(answer)), settings()).unwrap();
        s.command(surge(WEB, true)).await.unwrap();
        let before = cluster(&s);
        s.command(Command::Step).await.unwrap();
        settle().await;
        s.command(Command::Step).await.unwrap();
        let view = s.view();
        let decision = view.decisions.last().unwrap();
        assert_eq!(decision.status, status);
        assert_eq!(decision.code.as_deref(), Some(code));
        assert_eq!(before, cluster(&s), "{code}");
        assert_eq!(view.pending_pods, 8);
        assert_eq!(
            view.calls, 1,
            "no retry: the next evaluation waits for its cadence"
        );
        assert_eq!(view.cost.calls, 1);
        assert!(matches!(
            s.timeline().last(),
            Some(TimelineEvent::Decision(d)) if d.status == status
        ));
    }
}

#[tokio::test]
async fn reset_and_scenario_changes_cancel_a_pending_evaluation() {
    let slow = Slow::new(scale_up);
    let mut s = Session::new(42, Some(slow.clone()), settings()).unwrap();
    for cancel in [Command::Reset, scenario(Scenario::SurgeRecovery)] {
        s.command(surge(WEB, true)).await.unwrap();
        s.command(Command::Step).await.unwrap();
        settle().await;
        assert!(s.view().pending.is_some());
        let started = slow.started.load(Ordering::SeqCst);
        s.command(cancel).await.unwrap();
        let view = s.view();
        assert!(view.pending.is_none() && view.paused);
        assert_eq!((view.at_ms, view.calls, view.pending_pods), (0, 0, 0));
        assert!(view.history.is_empty() && s.timeline().is_empty());
        // Releasing the provider now reaches no one: the task was aborted.
        slow.release.notify_one();
        settle().await;
        assert_eq!(slow.finished.load(Ordering::SeqCst), 0);
        assert_eq!(slow.started.load(Ordering::SeqCst), started);
        assert!(s.view().decisions.is_empty());
        // Consume the stored permit so the next round parks again.
        slow.release.notified().await;
    }
    assert_eq!(s.view().scenario, Scenario::SurgeRecovery);
    // Estimated cost counts the calls already made; a reset does not refund them.
    assert_eq!(s.view().cost.calls, 2);
}

#[tokio::test]
async fn jev_is_shown_refused_removals_but_not_cooldown_refusals() {
    fn shrink(e: &Evidence) -> Inference {
        let removal = e
            .legal_actions
            .iter()
            .find(|c| matches!(c, Choice::Remove { .. }));
        Inference::chose(*removal.unwrap(), Some(0.9))
    }
    // The packed starting cluster has nowhere to move small-1's pod.
    let judge = Scripted::new(shrink);
    let mut s = Session::new(42, Some(judge.clone()), settings()).unwrap();
    let before = cluster(&s);
    for _ in 0..12 {
        s.command(Command::Step).await.unwrap();
        settle().await;
    }
    assert_eq!(before, cluster(&s));
    assert!(s
        .decisions()
        .iter()
        .all(|d| d.status == "rejected" && d.code.as_deref() == Some("drainable")));
    let seen = judge.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert!(seen[0].recent_rejections.is_empty());
    let refused = &seen[1].recent_rejections;
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].action.label(), "remove:small-1");
    assert_eq!(
        (refused[0].guard.as_str(), refused[0].seconds_ago),
        ("drainable", 4)
    );
    assert_eq!(seen[2].recent_rejections.len(), 2);

    // A cooldown refusal says nothing once the cooldown has passed, so it is not repeated back.
    let judge = Scripted::new(shrink);
    let mut s = Session::new(42, Some(judge.clone()), settings()).unwrap();
    s.command(surge(WEB, true)).await.unwrap();
    for _ in 0..7 {
        s.command(Command::Step).await.unwrap();
        settle().await;
    }
    let decision = &s.decisions()[0];
    assert_eq!(decision.code.as_deref(), Some("scale_down_cooldown"));
    assert!(judge.seen.lock().unwrap()[1].recent_rejections.is_empty());
}

fn scheduled(s: &Session) -> Vec<(u64, String)> {
    s.timeline()
        .iter()
        .filter_map(|e| match e {
            TimelineEvent::Control(c) => {
                assert_eq!(c.source, "preset");
                Some((c.at_ms, serde_json::to_string(&c.control).unwrap()))
            }
            TimelineEvent::Demand {
                at_ms,
                workload,
                replicas,
            } => Some((*at_ms, format!("{workload}={replicas}"))),
            _ => None,
        })
        .collect()
}
/// The export without its run tag, which is unique to every run by design.
fn exported(s: &Session) -> serde_json::Value {
    let mut export = s.export();
    let run = export.as_object_mut().unwrap().remove("simulation_run");
    assert_eq!(run.unwrap(), s.simulation_run());
    export
}
async fn complete(preset: Scenario, seed: u64, tick: u64) -> Session {
    let mut s = Session::new(seed, None, settings()).unwrap();
    s.command(scenario(preset)).await.unwrap();
    s.command(Command::Play).await.unwrap();
    run(&mut s, HORIZON_MS, tick).await;
    let view = s.view();
    assert!(view.paused);
    assert_eq!(view.at_ms, HORIZON_MS);
    assert_eq!(view.history.len(), 600);
    assert!(s.command(Command::Play).await.is_err());
    s
}
fn pending_at(s: &Session, at_ms: u64) -> usize {
    let view = s.view();
    view.history
        .iter()
        .find(|p| p.at_ms == at_ms)
        .unwrap()
        .pending_pods
}

#[tokio::test]
async fn presets_apply_their_changes_at_exact_simulated_times() {
    let live = complete(Scenario::Live, 42, 1000).await;
    assert!(scheduled(&live).is_empty());
    assert!(live.view().history.iter().all(|p| p.pending_pods == 0));

    let s = complete(Scenario::SurgeRecovery, 42, 1000).await;
    let control = |kind: &str, workload: usize, enabled: bool| {
        format!(r#"{{"kind":"{kind}","workload":{workload},"enabled":{enabled}}}"#)
    };
    assert_eq!(
        scheduled(&s),
        [
            (45_000, control("replica_surge", WEB, true)),
            (90_000, control("replica_surge", API, true)),
            (240_000, control("replica_surge", WEB, false)),
            (300_000, control("replica_surge", API, false)),
        ]
    );
    // Without Jev nothing provisions, so the surge waits until it ends.
    assert_eq!(
        [44_000, 45_000, 90_000, 240_000, 300_000].map(|at| pending_at(&s, at)),
        [0, 8, 12, 4, 0]
    );

    let s = complete(Scenario::MemoryHeavyBurst, 42, 1000).await;
    assert_eq!(
        scheduled(&s),
        [
            (45_000, control("memory_heavy", API, true)),
            (150_000, control("memory_heavy", BATCH, true)),
            (330_000, control("memory_heavy", API, false)),
            (390_000, control("memory_heavy", BATCH, false)),
        ]
    );
    assert_eq!(
        [44_000, 45_000, 150_000, 330_000, 390_000].map(|at| pending_at(&s, at)),
        [0, 2, 3, 1, 0]
    );

    let s = complete(Scenario::CyclicalLoad, 42, 1000).await;
    let changes = scheduled(&s);
    for cycle in 0..HORIZON_MS / CYCLE_MS {
        let start = cycle * CYCLE_MS;
        let in_cycle = |workload: usize| -> Vec<(u64, String)> {
            changes
                .iter()
                .filter(|(at, change)| {
                    (start..start + CYCLE_MS).contains(at)
                        && change.starts_with(&format!("{workload}="))
                })
                .map(|(at, change)| (at - start, change.clone()))
                .collect()
        };
        assert_eq!(
            in_cycle(API),
            [
                (40_000, "1=3"),
                (45_000, "1=4"),
                (80_000, "1=3"),
                (85_000, "1=2")
            ]
            .map(|(at, change)| (at, change.to_owned())),
            "cycle {cycle}"
        );
        let web = in_cycle(WEB);
        let peak = cycle_replicas(WEB, start + 60_000, 42);
        assert!((9..=11).contains(&peak));
        assert_eq!(web.len(), 4);
        assert_eq!(web[1], (45_000, format!("0={peak}")));
        assert_eq!(web[3], (85_000, "0=4".to_owned()));
    }
    assert_eq!(s.view().forecast.minimum_history_seconds, 180);
}

#[tokio::test]
async fn the_same_seed_gives_the_same_run_at_any_tick_size() {
    for preset in [Scenario::SurgeRecovery, Scenario::CyclicalLoad] {
        let coarse = exported(&complete(preset, 42, 1000).await);
        for tick in [50, 137] {
            let fine = exported(&complete(preset, 42, tick).await);
            assert_eq!(coarse, fine, "{preset:?} at {tick} ms");
        }
        let other = exported(&complete(preset, 7, 1000).await);
        assert_eq!(
            coarse["history"] == other["history"],
            preset == Scenario::SurgeRecovery,
            "only the cyclical preset draws its load from the seed"
        );
    }
    // Playback speed changes how fast time passes, not what happens.
    let mut s = Session::new(42, None, settings()).unwrap();
    s.command(scenario(Scenario::CyclicalLoad)).await.unwrap();
    assert!(s.command(Command::Speed { value: 3 }).await.is_err());
    s.command(Command::Speed { value: 4 }).await.unwrap();
    s.command(Command::Play).await.unwrap();
    run(&mut s, HORIZON_MS, 50).await;
    assert_eq!(
        s.export()["state"],
        complete(Scenario::CyclicalLoad, 42, 1000).await.export()["state"]
    );
}

#[tokio::test]
async fn a_scripted_judge_scales_up_then_down_through_the_guards() {
    let judge = Scripted::new(reactive);
    let mut s = Session::new(42, Some(judge.clone()), settings()).unwrap();
    s.command(scenario(Scenario::SurgeRecovery)).await.unwrap();
    s.command(Command::Play).await.unwrap();
    run(&mut s, 230_000, 250).await;
    let d = s.data();
    invariant(&s.phase(), &d).unwrap();
    assert_eq!(d.pending_count(), 0, "the surge is served");
    assert!(d.count(Some(Group::GeneralLarge), NodePhase::Ready) > 2);
    let peak_nodes = d.active_nodes();

    run(&mut s, 370_000, 250).await;
    let (phase, d) = (s.phase(), s.data());
    invariant(&phase, &d).unwrap();
    assert_eq!(d.pending_count(), 0);
    assert!(d.active_nodes() < peak_nodes, "empty nodes were removed");
    assert!(d.nodes.iter().any(|n| n.phase == NodePhase::Removed));

    let decisions = s.decisions();
    let count = |status: &str, code: Option<&str>| {
        decisions
            .iter()
            .filter(|d| d.status == status && d.code.as_deref() == code)
            .count()
    };
    assert!(count("applied", None) >= 4);
    assert!(count("unchanged", None) > 0);
    // The only refusals are recommendations overtaken by a node becoming ready.
    assert!(decisions
        .iter()
        .filter(|d| d.status == "rejected")
        .all(|d| d.code.as_deref() == Some("fresh")));
    assert_eq!(
        s.view().calls,
        decisions.len() + s.view().pending.iter().len()
    );

    // A node becoming ready or drained prompts a fresh look without waiting for the cadence.
    let ready_at = s.timeline().iter().find_map(|e| match e {
        TimelineEvent::Transition(t) if t.trigger.ends_with(" ready") => Some(t.at_ms),
        _ => None,
    });
    let ready_at = ready_at.unwrap();
    assert!(judge
        .seen
        .lock()
        .unwrap()
        .iter()
        .any(|e| (ready_at..ready_at + 500).contains(&e.observed_at_ms)));

    // Every call had a real choice to make, and none was made during a drain.
    let removals: Vec<u64> = decisions
        .iter()
        .filter(|d| d.status == "applied" && matches!(d.choice, Some(Choice::Remove { .. })))
        .map(|d| d.at_ms)
        .collect();
    assert!(!removals.is_empty());
    for evidence in judge.seen.lock().unwrap().iter() {
        assert!(evidence.legal_actions.len() > 1);
        assert!(!removals
            .iter()
            .any(|at| (*at..at + DRAIN_MS).contains(&evidence.observed_at_ms)
                && evidence.observed_at_ms > *at));
        // Jev sees what the cluster looks like, never why or what comes next.
        let text = serde_json::to_string(evidence).unwrap();
        for hidden in ["scenario", "surge", "script", "seed", "stockout"] {
            assert!(!text.contains(hidden), "{hidden}");
        }
        if evidence.observed_at_ms < 200_000 {
            assert!(!text.contains("240000") && !text.contains("300000"));
        }
    }
    // The decision record carries the exact evidence, the reply and the guard result.
    let exported = s.export();
    assert_eq!(exported["model"], "reflex-autoscaler-v1");
    assert_eq!(exported["scenario"], "surge_recovery");
    let first = &exported["decisions"][0];
    assert_eq!(first["evidence"]["observed_at_ms"], first["observed_at_ms"]);
    assert!(first["evidence"]["legal_actions"].is_array());
    assert!(first["result"]["confidence"].is_number());
    assert!(first["reason"].is_string() && first["status"].is_string());
    let applied = exported["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["status"] == "applied")
        .unwrap();
    assert_eq!(applied["choice"], "scale_up:general_large:2");
    assert_eq!(applied["to"], "scaling_up");
    let transitions = exported["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "transition")
        .count();
    assert!(transitions >= 8);
}

#[tokio::test]
async fn commands_validate_their_input() {
    let mut s = Session::new(42, None, settings()).unwrap();
    assert!(!s.view().available);
    assert!(s.command(surge(9, true)).await.is_err());
    assert!(s
        .command(Command::Forecast { enabled: true })
        .await
        .is_err());
    s.command(Command::Forecast { enabled: false })
        .await
        .unwrap();
    let stockout = Command::Control {
        control: Control::Stockout {
            group: Group::MemoryHeavy,
            enabled: true,
        },
    };
    s.command(stockout).await.unwrap();
    assert!(s.view().groups[2].group.stockout);
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.view().at_ms, 1000);
    assert!(s.view().paused);
    assert_eq!(s.view().calls, 0, "no evaluator, no calls");
    assert!(matches!(
        s.timeline(),
        [TimelineEvent::Control(c)] if c.source == "user" && c.at_ms == 0
    ));
    let parsed: Command = serde_json::from_str(
        r#"{"type":"control","control":{"kind":"memory_heavy","workload":2,"enabled":true}}"#,
    )
    .unwrap();
    s.command(parsed).await.unwrap();
    assert_eq!(s.view().pending_pods, 1);
    assert!(
        serde_json::from_str::<Command>(r#"{"type":"control","control":{"kind":"drain"}}"#)
            .is_err()
    );
    assert!(Session::new(
        42,
        None,
        JevSettings {
            model: " ".into(),
            ..settings()
        }
    )
    .is_err());
}

#[tokio::test]
async fn http_routes_serve_the_page_state_commands_and_export() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let shared = Arc::new(tokio::sync::Mutex::new(
        Session::new(42, None, settings()).unwrap(),
    ));
    let app = reflex_sim::autoscaler::web::router(shared.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    let get = |path: &str| client.get(format!("{url}{path}")).send();
    let post = |body: serde_json::Value| {
        client
            .post(format!("{url}/api/autoscaler/command"))
            .json(&body)
            .send()
    };

    let page = get("/autoscaler").await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let page = page.text().await.unwrap();
    assert!(page.contains("Cluster Autoscaler"));
    for (asset, content_type) in [
        ("/autoscaler.js", "text/javascript"),
        ("/autoscaler.css", "text/css"),
        ("/autoscaler-ui.js", "text/javascript"),
        ("/autoscaler-ui.css", "text/css"),
    ] {
        assert!(page.contains(asset), "{asset} is linked from the page");
        let response = get(asset).await.unwrap();
        assert_eq!(response.status(), 200, "{asset}");
        assert_eq!(response.headers()["content-type"], content_type);
        assert!(!response.text().await.unwrap().is_empty());
    }

    let state: serde_json::Value = get("/api/autoscaler/state")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["paused"], true);
    assert_eq!(state["phase"], "stable");
    assert_eq!(state["horizon_ms"], 600_000);
    assert_eq!(state["nodes"].as_array().unwrap().len(), 4);
    assert_eq!(state["pods"].as_array().unwrap().len(), 7);
    assert_eq!(state["scenario"], "live");

    let stepped = post(serde_json::json!({"type":"step"})).await.unwrap();
    assert_eq!(stepped.status(), 200);
    let stepped: serde_json::Value = stepped.json().await.unwrap();
    assert_eq!(stepped["at_ms"], 1000);
    let control = serde_json::json!({"type":"control","control":{"kind":"replica_surge","workload":0,"enabled":true}});
    let surged: serde_json::Value = post(control).await.unwrap().json().await.unwrap();
    assert_eq!(surged["pending_pods"], 8);
    assert_eq!(surged["workloads"][0]["desired"], 12);
    assert_eq!(surged["controls"][0]["source"], "user");
    let preset = serde_json::json!({"type":"scenario","scenario":"memory_heavy_burst"});
    let preset: serde_json::Value = post(preset).await.unwrap().json().await.unwrap();
    assert_eq!(
        (&preset["at_ms"], &preset["pending_pods"]),
        (&0.into(), &0.into())
    );
    assert_eq!(preset["scenario"], "memory_heavy_burst");
    let playing: serde_json::Value = post(serde_json::json!({"type":"play"}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(playing["paused"], false);

    for invalid in [
        serde_json::json!({"type":"speed","value":3}),
        serde_json::json!({"type":"control","control":{"kind":"replica_surge","workload":9,"enabled":true}}),
        serde_json::json!({"type":"forecast","enabled":true}),
    ] {
        let response = post(invalid).await.unwrap();
        assert_eq!(response.status(), 400);
        let body: serde_json::Value = response.json().await.unwrap();
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("invalid simulation"));
    }
    let unknown = post(serde_json::json!({"type":"replay"})).await.unwrap();
    assert!(unknown.status().is_client_error());
    assert_eq!(shared.lock().await.view().speed, 1);

    let export = get("/api/autoscaler/export").await.unwrap();
    assert_eq!(
        export.headers()["content-disposition"],
        "attachment; filename=reflex-autoscaler.json"
    );
    let export: serde_json::Value = export.json().await.unwrap();
    assert_eq!(export["model"], "reflex-autoscaler-v1");
    assert_eq!(export["scenario"], "memory_heavy_burst");
    assert_eq!(export["state"]["nodes"].as_array().unwrap().len(), 4);
    assert!(export["timeline"].is_array() && export["decisions"].is_array());
    server.abort();
}
