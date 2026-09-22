use reflex_sim::{
    engine::simulate,
    generate_trace,
    policy::{
        builtins, Admission, AdmissionContext, CircuitPhase, ClientOutcome, Observation, Policy,
        ThresholdConfig, ThresholdPolicy,
    },
    presets,
    scenario::{Change, Phase, Scenario},
    trace::{Request, Trace},
};
fn tiny() -> Scenario {
    let mut s = presets().remove(0);
    s.services.truncate(1);
    s.services[0].workers = 1;
    s.services[0].work_ms = 100.0;
    s.services[0].rate = 0.0;
    s.services[0].base_error_probability = 0.0;
    s.duration_ms = 1000.0;
    s.timeout_ms = 500.0;
    s.sample_ms = 50.0;
    s.stress_error_probability = 0.0;
    s.phases = vec![Phase {
        name: "Control".into(),
        start_ms: 0.0,
        end_ms: s.duration_ms,
        description: String::new(),
        changes: vec![],
    }];
    s
}
fn trace(s: &Scenario, arrivals: &[(f64, f64)]) -> Trace {
    Trace {
        model_version: "queue-stress-v1".into(),
        scenario: s.id.clone(),
        seed: 1,
        fingerprint: "fixture".into(),
        requests: arrivals
            .iter()
            .enumerate()
            .map(|(id, (at, work))| Request {
                id,
                at_ms: *at,
                work_ms: *work,
                service: 0,
                error_roll: 0.5,
            })
            .collect(),
    }
}
#[tokio::test]
async fn fifo_queue_and_worker_time_are_exact() {
    let s = tiny();
    let r = simulate(
        &s,
        &trace(&s, &[(0.0, 100.0), (10.0, 100.0), (20.0, 100.0)]),
        builtins()[0],
    )
    .await
    .unwrap();
    assert_eq!(
        r.requests
            .iter()
            .map(|r| r.downstream_finished_ms.unwrap())
            .collect::<Vec<_>>(),
        vec![100.0, 200.0, 300.0]
    );
    assert_eq!(r.requests[1].started_ms, Some(100.0));
    assert_eq!(r.metrics.work_ms, 300.0);
    assert_eq!(r.metrics.counts.success, 3);
    assert_eq!(r.metrics.peak_queue, 2);
}
#[tokio::test]
async fn timeout_does_not_cancel_work_or_report_late_success_twice() {
    let mut s = tiny();
    s.timeout_ms = 50.0;
    let r = simulate(&s, &trace(&s, &[(0.0, 100.0)]), builtins()[0])
        .await
        .unwrap();
    assert_eq!(r.metrics.counts.timeout, 1);
    assert_eq!(r.metrics.counts.success, 0);
    assert_eq!(r.requests[0].client_finished_ms, Some(50.0));
    assert_eq!(r.requests[0].downstream_finished_ms, Some(100.0));
    assert_eq!(r.requests[0].downstream_success, Some(true));
    assert_eq!(r.metrics.work_ms, 100.0);
    assert_eq!(r.metrics.post_timeout_work_ms, 50.0);
}
#[tokio::test]
async fn queued_timeouts_also_keep_their_place_in_queue() {
    let mut s = tiny();
    s.timeout_ms = 50.0;
    let r = simulate(
        &s,
        &trace(&s, &[(0.0, 100.0), (10.0, 100.0)]),
        builtins()[0],
    )
    .await
    .unwrap();
    assert_eq!(r.metrics.counts.timeout, 2);
    assert_eq!(r.requests[1].started_ms, Some(100.0));
    assert_eq!(r.requests[1].downstream_finished_ms, Some(200.0));
    assert_eq!(r.requests[1].post_timeout_work_ms, 100.0);
}
#[tokio::test]
async fn completion_wins_a_tie_with_client_deadline() {
    let mut s = tiny();
    s.timeout_ms = 100.0;
    let r = simulate(&s, &trace(&s, &[(0.0, 100.0)]), builtins()[0])
        .await
        .unwrap();
    assert_eq!(r.metrics.counts.success, 1);
    assert_eq!(r.metrics.counts.timeout, 0);
}
#[tokio::test]
async fn faults_change_processing_speed_of_existing_work() {
    let mut s = tiny();
    s.phases = vec![s.phases[0].clone(); 3];
    s.phases[0].end_ms = 50.0;
    s.phases[1].start_ms = 50.0;
    s.phases[1].end_ms = 150.0;
    s.phases[1].changes = vec![Change {
        service: s.services[0].id.clone(),
        rate_multiplier: 1.0,
        latency_multiplier: 4.0,
        error_probability: 0.0,
    }];
    s.phases[2].start_ms = 150.0;
    let r = simulate(&s, &trace(&s, &[(0.0, 100.0)]), builtins()[0])
        .await
        .unwrap();
    assert!((r.requests[0].downstream_finished_ms.unwrap() - 175.0).abs() < 1e-8);
}
#[tokio::test]
async fn full_queue_errors_are_distinct_from_circuit_shedding() {
    let mut s = tiny();
    s.services[0].queue_limit = 1;
    let r = simulate(
        &s,
        &trace(&s, &[(0.0, 200.0), (1.0, 200.0), (2.0, 200.0)]),
        builtins()[0],
    )
    .await
    .unwrap();
    assert_eq!(r.metrics.counts.error, 1);
    assert_eq!(r.metrics.counts.shed, 0);
    assert_eq!(r.requests[2].work_ms, 0.0);
    assert_eq!(
        r.requests[2].cause.as_deref(),
        Some("Downstream queue full")
    );
}
#[tokio::test]
async fn failed_requests_consume_worker_time() {
    let mut s = tiny();
    s.services[0].base_error_probability = 1.0;
    let r = simulate(&s, &trace(&s, &[(0.0, 100.0)]), builtins()[0])
        .await
        .unwrap();
    assert_eq!(r.metrics.counts.error, 1);
    assert_eq!(r.metrics.work_ms, 100.0);
    assert_eq!(r.metrics.wasted_work_ms, 100.0);
}
#[tokio::test]
async fn extra_load_raises_failure_risk_and_recovery_has_memory() {
    let mut s = tiny();
    s.stress_build_ms = 10.0;
    s.stress_recovery_ms = 1000.0;
    s.stress_error_probability = 1.0;
    s.timeout_ms = 10000.0;
    let light = simulate(&s, &trace(&s, &[(0.0, 100.0)]), builtins()[0])
        .await
        .unwrap();
    let heavy = simulate(
        &s,
        &trace(
            &s,
            &[
                (0.0, 100.0),
                (1.0, 100.0),
                (2.0, 100.0),
                (3.0, 100.0),
                (4.0, 100.0),
            ],
        ),
        builtins()[0],
    )
    .await
    .unwrap();
    assert_eq!(light.metrics.counts.error, 0);
    assert!(heavy.metrics.counts.error > 0);
    assert!(heavy.metrics.peak_stress > light.metrics.peak_stress);
    let at600 = heavy.snapshots.iter().find(|p| p.at_ms == 600.0).unwrap();
    assert_eq!(at600.active + at600.queued, 0);
    assert!(at600.stress > 0.1);
}
#[tokio::test]
async fn all_work_drains_after_arrival_horizon() {
    let mut s = tiny();
    s.duration_ms = 100.0;
    s.phases[0].end_ms = 100.0;
    let r = simulate(&s, &trace(&s, &[(90.0, 200.0)]), builtins()[0])
        .await
        .unwrap();
    assert_eq!(r.drained_at_ms, 290.0);
    assert_eq!(r.metrics.counts.success, 1);
    assert_eq!(r.snapshots.last().unwrap().at_ms, 100.0);
}
#[tokio::test]
async fn no_traffic_is_a_valid_control() {
    let s = tiny();
    let r = simulate(&s, &generate_trace(&s, 1).unwrap(), builtins()[1])
        .await
        .unwrap();
    assert_eq!(r.metrics.counts.offered, 0);
    assert_eq!(r.metrics.success_p95_ms, None);
}
#[tokio::test]
async fn same_seed_and_policy_replay_identically() {
    let mut s = presets().remove(1);
    s.duration_ms = 5000.0;
    s.phases = vec![Phase {
        name: "Control".into(),
        start_ms: 0.0,
        end_ms: 5000.0,
        description: String::new(),
        changes: vec![],
    }];
    let a = generate_trace(&s, 42).unwrap();
    let b = generate_trace(&s, 42).unwrap();
    assert_eq!(
        serde_json::to_value(&a).unwrap(),
        serde_json::to_value(&b).unwrap()
    );
    assert_ne!(a.fingerprint, generate_trace(&s, 43).unwrap().fingerprint);
    let first = simulate(&s, &a, builtins()[1]).await.unwrap();
    let second = simulate(&s, &a, builtins()[1]).await.unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(second).unwrap()
    );
}
#[test]
fn every_preset_is_valid_and_faults_are_explicit() {
    let scenarios = presets();
    assert_eq!(scenarios.len(), 6);
    for s in scenarios {
        s.validate().unwrap();
        let trace = generate_trace(&s, 7).unwrap();
        assert!(!trace.requests.is_empty());
        assert!(trace.requests.windows(2).all(|r| r[0].at_ms <= r[1].at_ms));
    }
}
#[test]
fn malformed_scenarios_fail_before_simulation() {
    let mut s = tiny();
    s.phases[0].start_ms = 1.0;
    assert!(s.validate().is_err());
    s = tiny();
    s.services[0].workers = 0;
    assert!(s.validate().is_err());
    s = tiny();
    s.stress_error_probability = f64::NAN;
    assert!(s.validate().is_err());
}
fn observed(at: f64, outcome: ClientOutcome, generation: u64) -> Observation {
    Observation {
        at_ms: at,
        request_id: 0,
        outcome,
        latency_ms: 1.0,
        generation,
    }
}
#[tokio::test]
async fn reflex_breaker_opens_reserves_one_probe_and_ignores_old_generations() {
    let mut p = ThresholdPolicy::new(ThresholdConfig {
        window_ms: 1000.0,
        minimum_samples: 2,
        failure_ratio: 0.5,
        cooldown_ms: 100.0,
    })
    .unwrap();
    p.observe(observed(1.0, ClientOutcome::Error, 0))
        .await
        .unwrap();
    p.observe(observed(2.0, ClientOutcome::Error, 0))
        .await
        .unwrap();
    assert_eq!(p.phase(), CircuitPhase::Open);
    assert!(matches!(
        p.admit(AdmissionContext {
            at_ms: 50.0,
            request_id: 1
        })
        .await
        .unwrap(),
        Admission::Shed
    ));
    let Admission::Allow { generation, probe } = p
        .admit(AdmissionContext {
            at_ms: 102.0,
            request_id: 2,
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(probe);
    assert_eq!(p.phase(), CircuitPhase::HalfOpen);
    assert!(matches!(
        p.admit(AdmissionContext {
            at_ms: 102.0,
            request_id: 3
        })
        .await
        .unwrap(),
        Admission::Shed
    ));
    p.observe(observed(103.0, ClientOutcome::Success, 0))
        .await
        .unwrap();
    assert_eq!(p.phase(), CircuitPhase::HalfOpen);
    p.observe(observed(104.0, ClientOutcome::Success, generation))
        .await
        .unwrap();
    assert_eq!(p.phase(), CircuitPhase::Closed);
}
#[tokio::test]
async fn side_by_side_runs_conserve_the_same_offered_requests() {
    let s = presets().remove(1);
    let trace = generate_trace(&s, 42).unwrap();
    let mut results = vec![];
    for f in builtins() {
        let r = simulate(&s, &trace, f).await.unwrap();
        let c = &r.metrics.counts;
        assert_eq!(c.offered as usize, trace.requests.len());
        assert_eq!(c.success + c.error + c.timeout + c.shed, c.offered);
        assert_eq!(c.admitted + c.shed, c.offered);
        assert_eq!(r.trace_fingerprint, trace.fingerprint);
        results.push(r);
    }
    assert_eq!(
        results[0]
            .requests
            .iter()
            .map(|r| (r.id, r.arrived_ms, r.service))
            .collect::<Vec<_>>(),
        results[1]
            .requests
            .iter()
            .map(|r| (r.id, r.arrived_ms, r.service))
            .collect::<Vec<_>>()
    );
    assert!(results[1]
        .transitions
        .iter()
        .any(|t| t.to == CircuitPhase::Open));
    assert!(results[1]
        .transitions
        .iter()
        .any(|t| t.to == CircuitPhase::HalfOpen));
    assert!(results[1]
        .transitions
        .iter()
        .any(|t| t.from == CircuitPhase::HalfOpen && t.to == CircuitPhase::Closed));
}

#[tokio::test]
async fn invalid_manual_trace_draws_and_work_are_rejected() {
    let s = tiny();
    for (work, roll) in [(100.0, 1.0), (100.0, f64::NAN), (f64::MAX, 0.5)] {
        let mut t = trace(&s, &[(0.0, work)]);
        t.requests[0].error_roll = roll;
        assert!(simulate(&s, &t, builtins()[0]).await.is_err());
    }
}
