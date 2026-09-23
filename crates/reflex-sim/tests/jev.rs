// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use reflex_sim::{
    jev::{
        Choice, EvaluationFuture, Evaluator, Evidence, Inference, JevPolicy, LiveEvaluator,
        PolicyKind,
    },
    playground::{inference::JevSettings, Command, Session},
    policy::{Admission, AdmissionContext, CircuitPhase, ClientOutcome, Observation, Policy},
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
fn answer(choice: Choice) -> Inference {
    Inference {
        action: Some(choice),
        confidence: None,
        probabilities: BTreeMap::new(),
        model: Some("test-model".into()),
        usage: None,
        request_id: None,
        error: None,
        wall_latency_ms: 0.0,
    }
}
async fn observe(policy: &mut JevPolicy) {
    for i in 0..20 {
        policy
            .observe(Observation {
                at_ms: i as f64 * 20.0,
                request_id: i,
                outcome: ClientOutcome::Timeout,
                latency_ms: 650.0,
                generation: 0,
            })
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn model_actions_are_guarded_and_only_one_probe_is_reserved() {
    let mut policy = JevPolicy::new().unwrap();
    let empty = policy.evidence(0.0).unwrap();
    let rejected = policy
        .apply_model(0.0, empty.clone(), answer(Choice::Open))
        .await
        .unwrap();
    assert_eq!(rejected.status, "rejected");
    let illegal = policy
        .apply_model(0.0, empty, answer(Choice::Probe))
        .await
        .unwrap();
    assert_eq!(illegal.status, "rejected");
    observe(&mut policy).await;
    let evidence = policy.evidence(500.0).unwrap();
    assert_eq!(evidence.last_5_seconds.responses, 20);
    // Recommendations need not have a fabricated confidence to be applied.
    let open = policy
        .apply_model(550.0, evidence.clone(), answer(Choice::Open))
        .await
        .unwrap();
    assert_eq!(open.to, CircuitPhase::Open);
    let current = policy.evidence(600.0).unwrap();
    let early = policy
        .apply_model(600.0, current, answer(Choice::Probe))
        .await
        .unwrap();
    assert!(early.reason.contains("cooldown"));
    let current = policy.evidence(3600.0).unwrap();
    let stale = policy
        .apply_model(3600.0, evidence, answer(Choice::Probe))
        .await
        .unwrap();
    assert!(stale.reason.contains("stale_revision"));
    policy
        .apply_model(3600.0, current, answer(Choice::Probe))
        .await
        .unwrap();
    let first = policy
        .admit(AdmissionContext {
            at_ms: 3601.0,
            request_id: 100,
        })
        .await
        .unwrap();
    let Admission::Allow {
        generation,
        probe: true,
    } = first
    else {
        panic!("one probe must be allowed")
    };
    assert!(matches!(
        policy
            .admit(AdmissionContext {
                at_ms: 3602.0,
                request_id: 101
            })
            .await
            .unwrap(),
        Admission::Shed
    ));
    policy
        .observe(Observation {
            at_ms: 3603.0,
            request_id: 1,
            outcome: ClientOutcome::Success,
            latency_ms: 100.0,
            generation: 0,
        })
        .await
        .unwrap();
    assert_eq!(policy.phase(), CircuitPhase::HalfOpen);
    policy
        .observe(Observation {
            at_ms: 3700.0,
            request_id: 100,
            outcome: ClientOutcome::Success,
            latency_ms: 99.0,
            generation,
        })
        .await
        .unwrap();
    assert_eq!(policy.phase(), CircuitPhase::Closed);
}
#[tokio::test]
async fn stale_evidence_and_failed_inference_cannot_change_the_circuit() {
    let mut policy = JevPolicy::new().unwrap();
    observe(&mut policy).await;
    let evidence = policy.evidence(400.0).unwrap();
    let old = policy
        .apply_model(5500.0, evidence, answer(Choice::Open))
        .await
        .unwrap();
    assert!(old.reason.contains("stale_evidence"));
    let current = policy.evidence(5500.0).unwrap();
    let failure = policy
        .apply_model(
            5500.0,
            current,
            Inference::failed("timeout", "deadline exceeded"),
        )
        .await
        .unwrap();
    assert_eq!(failure.status, "evaluation_error");
    assert_eq!(policy.phase(), CircuitPhase::Closed);
}
struct MockEvaluator {
    calls: AtomicUsize,
    delay: Duration,
}
impl Evaluator for MockEvaluator {
    fn evaluate(&self, evidence: Evidence) -> EvaluationFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            answer(if evidence.phase == CircuitPhase::Open {
                Choice::Probe
            } else if evidence.last_5_seconds.failure_ratio > 0.2 {
                Choice::Open
            } else {
                Choice::NoChange
            })
        })
    }
}
fn settings() -> JevSettings {
    JevSettings {
        model: "test-model".into(),
        dispatch_interval: Duration::ZERO,
    }
}
#[tokio::test]
async fn slow_inference_does_not_block_traffic_or_controls_and_reset_discards_it() {
    let mock = Arc::new(MockEvaluator {
        calls: AtomicUsize::new(0),
        delay: Duration::from_secs(30),
    });
    let mut session =
        Session::configured(42, PolicyKind::Jev, Some(mock.clone()), settings()).unwrap();
    session.command(Command::Play).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        session.tick(1000.0).await.unwrap();
        tokio::task::yield_now().await;
        assert!(session.view().inference.pending.is_some());
        session
            .command(Command::Fault {
                service: 1,
                faults: reflex_sim::engine::live::Faults {
                    slow: true,
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        session.tick(1000.0).await.unwrap();
    })
    .await
    .expect("provider wait must not block the session");
    assert!(session.view().simulation.counts.offered > 50);
    session.command(Command::Reset).await.unwrap();
    assert!(session.view().inference.pending.is_none());
    assert!(session.view().decisions.is_empty());
    assert_eq!(session.view().policy, PolicyKind::Jev);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn recorded_model_replay_makes_no_calls_and_preserves_outcomes() {
    let mock = Arc::new(MockEvaluator {
        calls: AtomicUsize::new(0),
        delay: Duration::from_millis(1),
    });
    let mut session =
        Session::configured(42, PolicyKind::Jev, Some(mock.clone()), settings()).unwrap();
    session
        .command(Command::Fault {
            service: 1,
            faults: reflex_sim::engine::live::Faults {
                slow: true,
                errors: true,
                surge: true,
            },
        })
        .await
        .unwrap();
    session.command(Command::Play).await.unwrap();
    for i in 0..80 {
        session.tick(250.0).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        if i == 35 {
            session.command(Command::Repair).await.unwrap();
        }
    }
    session.command(Command::Pause).await.unwrap();
    let original = session.view();
    assert!(original
        .decisions
        .iter()
        .any(|d| d.guard.status == "applied"));
    session.command(Command::Replay).await.unwrap();
    let calls = mock.calls.load(Ordering::SeqCst);
    for _ in 0..50 {
        session.tick(500.0).await.unwrap();
        tokio::task::yield_now().await;
    }
    let replay = session.view();
    assert_eq!(calls, mock.calls.load(Ordering::SeqCst));
    assert!(replay.paused);
    assert_eq!(replay.simulation.at_ms, original.simulation.at_ms);
    assert_eq!(
        serde_json::to_value(replay.simulation.counts).unwrap(),
        serde_json::to_value(original.simulation.counts).unwrap()
    );
    assert_eq!(
        serde_json::to_value(replay.decisions).unwrap(),
        serde_json::to_value(original.decisions).unwrap()
    );
}
#[tokio::test]
async fn pausing_still_bounds_inference_without_a_call_cap() {
    let mock = Arc::new(MockEvaluator {
        calls: AtomicUsize::new(0),
        delay: Duration::from_millis(1),
    });
    let config = settings();
    let mut session = Session::configured(42, PolicyKind::Jev, Some(mock.clone()), config).unwrap();
    session.command(Command::Step).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    session.tick(1000.0).await.unwrap();
    assert!(session.view().decisions.is_empty());
    assert!(session.view().inference.pending.unwrap().response_ready);
    session.command(Command::Step).await.unwrap();
    assert_eq!(session.view().decisions.len(), 1);
    for _ in 0..10 {
        session.command(Command::Step).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(mock.calls.load(Ordering::SeqCst) > 1);
    let calls = session.view().inference.calls;
    session.tick(1000.0).await.unwrap();
    assert_eq!(session.view().inference.calls, calls);
    assert!(Session::configured(42, PolicyKind::Jev, None, settings()).is_err());
}

#[tokio::test]
async fn real_sdk_wire_roundtrip_keeps_scores_and_does_not_reuse_failed_diagnostics() {
    use axum::{routing::post, Json, Router};
    let seen = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let capture = seen.clone();
    let app=Router::new().route("/systemone",post(move |Json(body):Json<serde_json::Value>| {let capture=capture.clone();async move {
        let mut calls=capture.lock().unwrap();calls.push(body);
        if calls.len()==1 { (axum::http::StatusCode::OK,Json(serde_json::json!({"model":"jev-test-resolved","usage":{"input_tokens":25,"output_tokens":2},"answers":{"action":{"type":"choice","choice":"no_change","probabilities":{"open":0.1,"probe":0.1,"no_change":0.8}}}})))}
        else {(axum::http::StatusCode::TOO_MANY_REQUESTS,Json(serde_json::json!({"error":"rate limited"}))) }
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/systemone", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = typesafe_ai::TypeSafeClient::builder()
        .api_key("test-key")
        .endpoint(endpoint)
        .max_retries(0)
        .build()
        .unwrap();
    let evaluator = LiveEvaluator::new(client, "jev-test".into());
    let mut policy = JevPolicy::new().unwrap();
    observe(&mut policy).await;
    let evidence = policy.evidence(400.0).unwrap();
    let result = evaluator.evaluate(evidence.clone()).await;
    assert_eq!(result.action, Some(Choice::NoChange));
    assert_eq!(result.confidence, None);
    assert_eq!(result.probabilities["no_change"], 0.8);
    assert_eq!(result.model.as_deref(), Some("jev-test-resolved"));
    let failed = evaluator.evaluate(evidence).await;
    assert!(failed.error.is_some());
    assert!(failed.model.is_none());
    assert!(failed.probabilities.is_empty());
    let calls = seen.lock().unwrap();
    let state = &calls[0]["state"];
    assert!(state.get("last_5_seconds").is_some());
    for hidden in [
        "stress",
        "queued",
        "faults",
        "injected_error_probability",
        "error_roll",
    ] {
        assert!(state.get(hidden).is_none());
    }
    assert_eq!(calls[0]["model"], "jev-test");
    server.abort();
}

struct MeteredEvaluator;
impl Evaluator for MeteredEvaluator {
    fn evaluate(&self, _: Evidence) -> EvaluationFuture<'_> {
        Box::pin(async {
            let mut result = answer(Choice::NoChange);
            result.model = Some("jev-1.13.0".into());
            result.usage = Some(typesafe_ai::Usage {
                input_tokens: 1000,
                output_tokens: 40,
                extra: Default::default(),
            });
            result
        })
    }
}
#[tokio::test]
async fn cost_counts_paused_responses_once_and_survives_replay_reset_and_rejected_policy_changes() {
    let mut config = settings();
    config.dispatch_interval = Duration::from_secs(3600);
    let mut session = Session::configured(
        42,
        PolicyKind::Jev,
        Some(Arc::new(MeteredEvaluator)),
        config,
    )
    .unwrap();
    session.command(Command::Step).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !session
            .view()
            .inference
            .pending
            .as_ref()
            .is_some_and(|p| p.response_ready)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // The provider was paid even though the simulation is paused and no decision applied.
    assert!(session.view().paused);
    assert!(session.view().decisions.is_empty());
    let original = session.view().inference.cost;
    assert!((original.estimated_usd - 0.000042).abs() < 1e-12);
    assert_eq!(original.priced_calls, 1);
    assert_eq!(original.input_tokens, 1000);
    assert_eq!(original.output_tokens, 40);
    assert_eq!(original.missing_usage_calls, 0);
    session.command(Command::Step).await.unwrap();
    assert_eq!(session.view().decisions.len(), 1);
    session.command(Command::Replay).await.unwrap();
    for _ in 0..3 {
        session.tick(1000.0).await.unwrap();
    }
    session.command(Command::Reset).await.unwrap();
    assert_eq!(session.view().inference.calls, 0);
    assert!(session
        .command(Command::Policy {
            policy: PolicyKind::Threshold,
        })
        .await
        .is_err());
    assert_eq!(session.view().policy, PolicyKind::Jev);
    assert_eq!(
        serde_json::to_value(session.view().inference.cost).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}
