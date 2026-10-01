// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use opentelemetry::metrics::MeterProvider;
#[allow(dead_code)]
#[path = "support/metrics.rs"]
mod metrics;
use metrics::{count, gauge, histogram, Capture};
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use reflex_sim::{
    autoscaler::{
        engine::{Choice, Control, Group, NodePhase},
        judge::{Evaluation, Evaluator, Evidence, Inference, LiveEvaluator},
        Command, Session,
    },
    playground::inference::JevSettings,
};
use std::{sync::Arc, time::Duration};

const GIB: f64 = 1073741824.;
fn settings() -> JevSettings {
    JevSettings {
        dispatch_interval: Duration::ZERO,
        ..Default::default()
    }
}
async fn steps(session: &mut Session, count: usize) {
    for _ in 0..count {
        session.command(Command::Step).await.unwrap();
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }
}
fn surge(workload: usize, enabled: bool) -> Command {
    Command::Control {
        control: Control::ReplicaSurge { workload, enabled },
    }
}
type Answer = fn(&Evidence) -> Inference;
struct Scripted(Answer);
impl Evaluator for Scripted {
    fn evaluate(&self, evidence: Evidence) -> Evaluation<'_> {
        let inference = (self.0)(&evidence);
        Box::pin(async move { inference })
    }
}
fn seconds(metrics: &ResourceMetrics, name: &str) -> f64 {
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    metrics
        .scope_metrics()
        .flat_map(|s| s.metrics())
        .filter(|m| m.name() == name)
        .map(|m| match m.data() {
            AggregatedMetrics::F64(MetricData::Sum(sum)) => {
                sum.data_points().map(|p| p.value()).sum::<f64>()
            }
            _ => panic!("Expected a float counter: {name}"),
        })
        .sum()
}
fn names(metrics: &ResourceMetrics, prefix: &str) -> Vec<String> {
    let mut names: Vec<_> = metrics
        .scope_metrics()
        .flat_map(|s| s.metrics())
        .map(|m| m.name().to_owned())
        .filter(|n| n.starts_with(prefix))
        .collect();
    names.sort();
    names.dedup();
    names
}

#[tokio::test]
async fn gauges_describe_the_cluster_and_carry_policy_and_simulation_run() {
    let c = Capture::new();
    let mut s = Session::with_meter(42, None, settings(), c.provider.meter("autoscaler")).unwrap();
    let run = s.simulation_run().to_owned();
    assert!(run.starts_with("autoscaler-"));
    let scope = [("policy", "jev"), ("simulation_run", run.as_str())];
    let tagged = |extra: &[(&'static str, &'static str)]| -> Vec<(&str, &str)> {
        scope.iter().chain(extra).copied().collect()
    };
    let m = c.read();
    assert_eq!(
        names(&m, "autoscaler."),
        [
            "autoscaler.cost.rate",
            "autoscaler.cpu.ready",
            "autoscaler.cpu.requested",
            "autoscaler.group.cpu.capacity",
            "autoscaler.group.cpu.reserved",
            "autoscaler.group.memory.capacity",
            "autoscaler.group.memory.reserved",
            "autoscaler.memory.ready",
            "autoscaler.memory.requested",
            "autoscaler.nodes",
            "autoscaler.phase",
            "autoscaler.pods.pending",
            "autoscaler.pods.pending.oldest_age",
            "autoscaler.workload.replicas.available",
            "autoscaler.workload.replicas.desired",
        ]
    );
    for (name, expected) in [
        ("autoscaler.cpu.requested", 18.),
        ("autoscaler.memory.requested", 40. * GIB),
        ("autoscaler.cpu.ready", 24.),
        ("autoscaler.memory.ready", 48. * GIB),
    ] {
        assert_eq!(gauge(&m, name, &scope), vec![expected], "{name}");
        // Nothing is published without the run tag.
        assert_eq!(gauge(&m, name, &[]).len(), 1, "{name}");
    }
    let web = tagged(&[("workload", "web")]);
    assert_eq!(gauge(&m, "autoscaler.pods.pending", &web), vec![0.]);
    assert_eq!(
        gauge(&m, "autoscaler.workload.replicas.desired", &web),
        vec![4.]
    );
    assert_eq!(
        gauge(&m, "autoscaler.workload.replicas.available", &web),
        vec![4.]
    );
    assert_eq!(gauge(&m, "autoscaler.pods.pending", &scope).len(), 3);
    let large = tagged(&[("group", "general_large")]);
    assert_eq!(
        gauge(&m, "autoscaler.group.cpu.capacity", &large),
        vec![16.]
    );
    assert_eq!(
        gauge(&m, "autoscaler.group.cpu.reserved", &large),
        vec![14.]
    );
    assert_eq!(
        gauge(&m, "autoscaler.group.memory.capacity", &large),
        vec![32. * GIB]
    );
    assert_eq!(
        gauge(&m, "autoscaler.group.memory.reserved", &large),
        vec![30. * GIB]
    );
    assert_eq!(gauge(&m, "autoscaler.cost.rate", &large), vec![0.8]);
    // Every group and lifecycle state is reported, so an empty one reads zero rather than stale.
    assert_eq!(gauge(&m, "autoscaler.nodes", &scope).len(), 9);
    for (group, state, expected) in [
        ("general_small", "ready", 2.),
        ("general_large", "ready", 2.),
        ("memory_heavy", "ready", 0.),
        ("general_large", "provisioning", 0.),
        ("general_small", "draining", 0.),
    ] {
        let tags = tagged(&[("group", group), ("state", state)]);
        assert_eq!(
            gauge(&m, "autoscaler.nodes", &tags),
            vec![expected],
            "{group} {state}"
        );
    }
    assert_eq!(
        gauge(&m, "autoscaler.phase", &tagged(&[("phase", "stable")])),
        vec![1.]
    );
    assert_eq!(
        gauge(&m, "autoscaler.phase", &tagged(&[("phase", "scaling_up")])),
        vec![0.]
    );

    // Load changes show up as pending pods, their age, and accumulated pending time.
    steps(&mut s, 1).await;
    s.command(surge(0, true)).await.unwrap();
    steps(&mut s, 3).await;
    let m = c.read();
    assert_eq!(gauge(&m, "autoscaler.pods.pending", &web), vec![8.]);
    assert_eq!(
        gauge(&m, "autoscaler.pods.pending.oldest_age", &web),
        vec![3.]
    );
    assert_eq!(
        gauge(&m, "autoscaler.workload.replicas.desired", &web),
        vec![12.]
    );
    assert_eq!(gauge(&m, "autoscaler.cpu.requested", &scope), vec![34.]);
    assert_eq!(gauge(&m, "autoscaler.cpu.ready", &scope), vec![24.]);
    assert_eq!(
        seconds(&m, "autoscaler.pods.pending.time"),
        24.,
        "8 pods for 3 seconds"
    );
    // A paused export repeats the gauges and adds nothing to the counter.
    let paused = c.read();
    assert_eq!(gauge(&paused, "autoscaler.pods.pending", &web), vec![8.]);
    assert_eq!(seconds(&paused, "autoscaler.pods.pending.time"), 24.);

    // A reset is a new run: the old run's gauges stop, the new run starts from its own state.
    s.command(Command::Reset).await.unwrap();
    let next = s.simulation_run().to_owned();
    assert_ne!(next, run);
    let m = c.read();
    assert!(gauge(&m, "autoscaler.cpu.requested", &scope).is_empty());
    assert_eq!(
        gauge(
            &m,
            "autoscaler.cpu.requested",
            &[("simulation_run", next.as_str())]
        ),
        vec![18.]
    );
    drop(s);
    assert!(gauge(&c.read(), "autoscaler.nodes", &[]).is_empty());
}

/// Adds large nodes while pods are pending; otherwise tries to remove the first node offered.
fn grow_then_shrink(e: &Evidence) -> Inference {
    let large = Choice::ScaleUp {
        group: Group::GeneralLarge,
        count: 2,
    };
    let choice = if !e.pending_pods.is_empty() {
        (e.provisioning.is_empty() && e.legal_actions.contains(&large)).then_some(large)
    } else {
        e.legal_actions
            .iter()
            .copied()
            .find(|c| matches!(c, Choice::Remove { .. }))
    };
    Inference::chose(choice.unwrap_or(Choice::NoChange), Some(0.8))
}
#[tokio::test]
async fn decisions_node_lifecycle_and_waits_are_counted_once() {
    let c = Capture::new();
    let judge = Arc::new(Scripted(grow_then_shrink));
    let mut s =
        Session::with_meter(42, Some(judge), settings(), c.provider.meter("autoscaler")).unwrap();
    let run = s.simulation_run().to_owned();
    // The packed starting cluster: the first removal Jev tries cannot be drained.
    steps(&mut s, 3).await;
    let m = c.read();
    let rejected = [
        ("action", "remove"),
        ("outcome", "rejected"),
        ("guard", "drainable"),
        ("group", "general_small"),
        ("error", "true"),
        ("simulation_run", run.as_str()),
    ];
    assert_eq!(count(&m, "autoscaler.decisions", &rejected), 1);
    assert_eq!(count(&m, "autoscaler.decisions", &[]), 1);

    s.command(surge(2, true)).await.unwrap();
    steps(&mut s, 50).await;
    let d = s.data();
    assert_eq!(d.pending_count(), 0);
    assert_eq!(d.count(Some(Group::GeneralLarge), NodePhase::Ready), 4);
    let m = c.read();
    let applied = [
        ("action", "scale_up"),
        ("outcome", "applied"),
        ("group", "general_large"),
        ("error", "false"),
    ];
    assert_eq!(count(&m, "autoscaler.decisions", &applied), 1);
    assert_eq!(
        count(&m, "autoscaler.decisions", &[]) as usize,
        s.decisions().len()
    );
    let large = [("group", "general_large"), ("simulation_run", run.as_str())];
    for (event, expected) in [
        ("requested", 2),
        ("ready", 2),
        ("failed", 0),
        ("removed", 0),
    ] {
        let tags: Vec<_> = large.iter().copied().chain([("event", event)]).collect();
        assert_eq!(
            count(&m, "autoscaler.node.events", &tags),
            expected,
            "{event}"
        );
    }
    let (samples, seconds) = histogram(&m, "autoscaler.node.provision.duration", &large);
    assert_eq!(samples, 2);
    assert!(
        (50. ..=70.).contains(&seconds),
        "two nodes at 25-35s each: {seconds}"
    );
    // Both surge pods waited for those nodes; their wait is recorded when they are placed.
    let batch = [("workload", "batch"), ("simulation_run", run.as_str())];
    let (samples, seconds) = histogram(&m, "autoscaler.pod.pending.duration", &batch);
    assert_eq!(samples, 2);
    assert!(seconds > 50., "{seconds}");
    // Reading again counts nothing twice.
    let again = c.read();
    assert_eq!(
        count(&m, "autoscaler.node.events", &[]),
        count(&again, "autoscaler.node.events", &[])
    );
    assert_eq!(
        histogram(&m, "autoscaler.pod.pending.duration", &[]),
        histogram(&again, "autoscaler.pod.pending.duration", &[])
    );

    // An evaluation error is a decision outcome with no action.
    fn failed(_: &Evidence) -> Inference {
        Inference::failed("timeout", "inference deadline exceeded")
    }
    let c = Capture::new();
    let mut s = Session::with_meter(
        42,
        Some(Arc::new(Scripted(failed))),
        settings(),
        c.provider.meter("autoscaler"),
    )
    .unwrap();
    steps(&mut s, 3).await;
    let errors = [
        ("action", "none"),
        ("outcome", "evaluation_error"),
        ("error", "true"),
    ];
    assert_eq!(count(&c.read(), "autoscaler.decisions", &errors), 1);
}

/// A loopback TypeSafe endpoint that always answers `no_change`.
async fn typesafe() -> (String, tokio::task::JoinHandle<()>) {
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/systemone",
        post(|Json(body): Json<serde_json::Value>| async move {
            let labels = body["questions"]["action"]["criteria"].as_object().unwrap();
            let probabilities: serde_json::Map<_, _> = labels
                .keys()
                .map(|k| (k.clone(), serde_json::json!(if k == "no_change" { 1.0 } else { 0.0 })))
                .collect();
            Json(serde_json::json!({"model":"jev-test","usage":{"input_tokens":5,"output_tokens":1},
                "answers":{"action":{"type":"choice","choice":"no_change","probabilities":probabilities}}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/systemone", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (endpoint, server)
}
async fn decide(s: &mut Session) {
    s.command(Command::Step).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while s.view().pending.is_some_and(|p| !p.response_ready) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    s.command(Command::Step).await.unwrap();
    assert_eq!(s.decisions()[0].status, "unchanged");
}

#[tokio::test]
async fn the_autoscaler_telemetry_is_all_on_its_meter_or_not_exported_at_all() {
    let (endpoint, server) = typesafe().await;
    // Wired: session, machine, controller and TypeSafe client share one application meter.
    let wired = Capture::new();
    let client = typesafe_ai::TypeSafeClient::builder()
        .api_key("test-key")
        .endpoint(&endpoint)
        .max_retries(0)
        .name("cluster_autoscaler")
        .meter(wired.provider.meter("typesafe-ai"))
        .build()
        .unwrap();
    let judge =
        LiveEvaluator::new(client, "jev-test".into()).with_meter(wired.provider.meter("reflex"));
    let mut s = Session::with_meter(
        42,
        Some(Arc::new(judge)),
        settings(),
        wired.provider.meter("autoscaler"),
    )
    .unwrap();
    decide(&mut s).await;
    let m = wired.read();
    let named = [("controller", "cluster_autoscaler"), ("status", "proposed")];
    assert_eq!(count(&m, "reflex.evaluations", &named), 1);
    assert_eq!(
        count(&m, "reflex.evaluations", &[]),
        1,
        "every evaluation is attributable"
    );
    let machine = [("machine", "cluster_autoscaler")];
    let transitions = count(&m, "reflex.transitions", &machine);
    assert!(transitions > 0);
    assert_eq!(count(&m, "reflex.transitions", &[]), transitions);
    let calls = [("client", "cluster_autoscaler"), ("status", "success")];
    assert_eq!(count(&m, "typesafe.client.requests", &calls), 1);
    assert_eq!(count(&m, "typesafe.client.requests", &[]), 1);
    assert_eq!(
        count(
            &m,
            "typesafe.client.tokens",
            &[("client", "cluster_autoscaler")]
        ),
        6
    );
    assert_eq!(histogram(&m, "typesafe.client.call.duration", &calls).0, 1);
    assert_eq!(
        count(
            &m,
            "autoscaler.decisions",
            &[("action", "no_change"), ("outcome", "unchanged")]
        ),
        1
    );

    // Not wired: the same decision on the default meters. No exporter is installed in this
    // process, as when the playground runs without --datadog, so it is recorded nowhere:
    // in particular it does not leak into another session's meter.
    let client = typesafe_ai::TypeSafeClient::builder()
        .api_key("test-key")
        .endpoint(&endpoint)
        .max_retries(0)
        .name("cluster_autoscaler")
        .build()
        .unwrap();
    let judge = Arc::new(LiveEvaluator::new(client, "jev-test".into()));
    let mut s = Session::new(42, Some(judge), settings()).unwrap();
    decide(&mut s).await;
    assert_eq!(s.view().calls, 1);
    let m = wired.read();
    for name in [
        "reflex.evaluations",
        "typesafe.client.requests",
        "autoscaler.decisions",
    ] {
        assert_eq!(
            count(&m, name, &[]),
            1,
            "{name} still holds only the wired decision"
        );
    }
    assert_eq!(count(&m, "reflex.transitions", &[]), transitions);
    server.abort();
}
