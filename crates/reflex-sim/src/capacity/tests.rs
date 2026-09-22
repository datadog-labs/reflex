use super::*;
use forecast::{ForecastFuture, Series};
use std::sync::atomic::{AtomicUsize, Ordering};
fn snapshot(i: &Input) -> Snapshot {
    Snapshot {
        origin_ms: i.origin_ms,
        interval_ms: 1000,
        request_id: "test-fixture".into(),
        source: "test_fixture".into(),
        model_provenance: "synthetic unit test, not Toto".into(),
        quantiles: [0.1, 0.5, 0.9],
        series: (0..3)
            .map(|k| Series {
                median: vec![if k == 0 { 3. } else { 32. }; i.prediction_length],
                lower: vec![0.; i.prediction_length],
                upper: vec![if k == 0 { 4. } else { 40. }; i.prediction_length],
            })
            .collect(),
        latency_ms: 1.,
    }
}
#[test]
fn forecast_contract_rejects_malformed_evidence() {
    let h = workload::prehistory(42, Scenario::Cycles, &workload::defaults());
    let i = workload::input(&h, 0);
    assert!(i.validate().is_ok());
    let mut bad = i.clone();
    bad.timestamps[10] += 3;
    assert!(bad.validate().is_err());
    let mut bad = i.clone();
    bad.values[0].pop();
    assert!(bad.validate().is_err());
    let mut bad = i.clone();
    bad.values[0][0] = f32::NAN;
    assert!(bad.validate().is_err());
    let f = snapshot(&i);
    assert!(f.validate(&i).is_ok());
    assert!(f.fresh(30_000));
    assert!(!f.fresh(30_001));
    let mut bad = f.clone();
    bad.series[0].lower[1] = 100.;
    assert!(bad.validate(&i).is_err());
    let mut bad = f.clone();
    bad.series[1].upper.pop();
    assert!(bad.validate(&i).is_err());
    let mut bad = f;
    bad.origin_ms = 1000;
    assert!(bad.validate(&i).is_err());
}
#[tokio::test]
async fn reservations_startup_and_stale_proposals_are_guarded() {
    let mut e = Engine::new(Settings {
        startup_s: 5,
        max_nodes: 3,
    })
    .unwrap();
    let p = Proposal {
        action: Action::StartOne,
        observed_at: 0,
        revision: 0,
        forecast_origin: None,
    };
    assert!(e.apply(p.clone(), None).await.unwrap().0);
    assert!(!e.apply(p, None).await.unwrap().0);
    assert_eq!(e.data().count(Lifecycle::Starting), 1);
    let clients = vec![Client {
        id: 0,
        rate: 4.,
        cpu: 8,
        memory_gib: 16,
        duration_s: 10,
        enabled: true,
    }];
    for t in 1..=4 {
        e.event(Event::Tick(workload::bucket(
            42,
            Scenario::Cycles,
            &clients,
            t,
        )))
        .await
        .unwrap();
        let d = e.data();
        assert_eq!(d.nodes[2].used_cpu, 0);
        engine::invariant(&engine::Phase::Managing, &d).unwrap();
    }
    e.event(Event::Tick(workload::bucket(
        42,
        Scenario::Cycles,
        &clients,
        5,
    )))
    .await
    .unwrap();
    assert_eq!(e.data().nodes[2].phase, Lifecycle::Ready);
    assert!(e.data().nodes[2].used_cpu > 0);
    let d = e.data();
    assert!(
        !e.apply(
            Proposal {
                action: Action::StartTwo,
                observed_at: d.at_ms,
                revision: d.revision,
                forecast_origin: None
            },
            None
        )
        .await
        .unwrap()
        .0
    );
}
#[tokio::test]
async fn stale_forecast_cannot_change_capacity() {
    let mut e = Engine::new(Settings::default()).unwrap();
    for t in 1..=31 {
        e.event(Event::Tick(Bucket {
            at_ms: t * 1000,
            values: [0.; 3],
            jobs: vec![],
        }))
        .await
        .unwrap();
    }
    let d = e.data();
    let before = serde_json::to_value(&d).unwrap();
    assert!(
        !e.apply(
            Proposal {
                action: Action::StartOne,
                observed_at: d.at_ms,
                revision: d.revision,
                forecast_origin: Some(0)
            },
            None
        )
        .await
        .unwrap()
        .0
    );
    assert_eq!(before, serde_json::to_value(e.data()).unwrap());
}
#[tokio::test]
async fn no_forecast_is_explicit_identical_fallback_and_replay_matches() {
    let mut s = Session::new(42, Options::default(), JevSettings::default()).unwrap();
    for t in 0..100 {
        if t == 17 {
            s.command(Command::Settings {
                settings: Settings {
                    startup_s: 5,
                    max_nodes: 6,
                },
            })
            .await
            .unwrap();
        }
        if t == 40 {
            let mut c = s.clients[0].clone();
            c.rate = 3.;
            s.command(Command::Client { client: c }).await.unwrap();
        }
        s.command(Command::Step).await.unwrap();
    }
    let states: Vec<_> = s
        .engines
        .iter()
        .map(|e| serde_json::to_value(e.data()).unwrap())
        .collect();
    assert_eq!(states[0], states[1]);
    assert_eq!(states[1], states[2]);
    assert_eq!(s.forecast_calls, 0);
    assert!(s.decisions.iter().any(|d| d.source.contains("fallback")));
    s.command(Command::Replay).await.unwrap();
    for _ in 0..100 {
        s.command(Command::Step).await.unwrap();
    }
    for (i, e) in s.engines.iter().enumerate() {
        assert_eq!(states[i], serde_json::to_value(e.data()).unwrap());
    }
    assert_eq!(s.cost.lock().unwrap().calls, 0);
}
struct FakeForecast(Arc<AtomicUsize>);
impl Forecaster for FakeForecast {
    fn description(&self) -> String {
        "test fixture".into()
    }
    fn forecast(&self, i: Input) -> ForecastFuture<'_> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(snapshot(&i)) })
    }
}
struct FakeJudge(Arc<AtomicUsize>);
impl Evaluator for FakeJudge {
    fn evaluate(&self, e: Evidence) -> judge::Evaluation<'_> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let v = serde_json::to_string(&e).unwrap();
        assert!(!v.contains("actual_s"));
        assert!(!v.contains("scenario"));
        assert!(e.forecast.is_some());
        Box::pin(async {
            let mut r = Inference::failed("");
            r.error = None;
            r.choice = Some(Action::StartTwo);
            r.confidence = Some(0.8);
            r
        })
    }
}
#[tokio::test]
async fn forecast_jev_and_replay_share_exact_availability_without_calls() {
    let f = Arc::new(AtomicUsize::new(0));
    let j = Arc::new(AtomicUsize::new(0));
    let mut s = Session::new(
        42,
        Options {
            recording: None,
            forecaster: Some(Arc::new(FakeForecast(f.clone()))),
            evaluator: Some(Arc::new(FakeJudge(j.clone()))),
        },
        JevSettings::default(),
    )
    .unwrap();
    for _ in 0..45 {
        s.command(Command::Step).await.unwrap();
        tokio::task::yield_now().await;
    }
    assert_eq!(f.load(Ordering::SeqCst), 1);
    assert!(j.load(Ordering::SeqCst) > 0);
    assert!(s.decisions.iter().any(|d| d.source == "jev"));
    assert!(!s.latest.as_ref().unwrap().fresh(s.at()));
    let before: Vec<_> = s
        .engines
        .iter()
        .map(|e| serde_json::to_value(e.data()).unwrap())
        .collect();
    let accuracy = serde_json::to_value(&s.accuracy).unwrap();
    let fc = f.load(Ordering::SeqCst);
    let jc = j.load(Ordering::SeqCst);
    s.command(Command::Replay).await.unwrap();
    for _ in 0..45 {
        s.command(Command::Step).await.unwrap();
        tokio::task::yield_now().await;
    }
    assert_eq!(fc, f.load(Ordering::SeqCst));
    assert_eq!(jc, j.load(Ordering::SeqCst));
    assert_eq!(accuracy, serde_json::to_value(&s.accuracy).unwrap());
    for (i, e) in s.engines.iter().enumerate() {
        assert_eq!(before[i], serde_json::to_value(e.data()).unwrap());
    }
}
#[tokio::test]
async fn canned_scenarios_conserve_jobs_and_resources() {
    for scenario in [Scenario::Cycles, Scenario::Ramp, Scenario::Surprise] {
        let mut s = Session::new(7, Options::default(), JevSettings::default()).unwrap();
        s.command(Command::Scenario { scenario }).await.unwrap();
        for _ in 0..600 {
            s.command(Command::Step).await.unwrap();
        }
        for e in &s.engines {
            let d = e.data();
            engine::invariant(&engine::Phase::Managing, &d).unwrap();
            assert!(d.offered > 0);
            assert_eq!(d.jobs.len() as u64, d.offered);
            assert_eq!(d.at_ms, 600_000);
        }
        assert!(s.paused);
    }
}
#[test]
fn future_traffic_is_not_an_input() {
    let mut h = workload::prehistory(42, Scenario::Surprise, &workload::defaults());
    let i = workload::input(&h, 0);
    assert_eq!(i.values.len(), 256);
    assert_eq!(i.timestamps.last(), Some(&forecast::EPOCH));
    h.push(workload::bucket(
        42,
        Scenario::Surprise,
        &workload::defaults(),
        1,
    ));
    let n = workload::input(&h, 1000);
    assert_eq!(&i.values[1..], &n.values[..255]);
}
#[tokio::test]
async fn recordings_validate_round_trip_and_reject_future_leaks() {
    let mut s = Session::new(42, Options::default(), JevSettings::default()).unwrap();
    for _ in 0..5 {
        s.command(Command::Step).await.unwrap();
    }
    s.recording.validate().unwrap();
    let bytes = serde_json::to_vec(&s.recording).unwrap();
    let tape: Recording = serde_json::from_slice(&bytes).unwrap();
    s.load_recording(tape).unwrap();
    assert!(s.replay.is_some());
    let mut bad = s.replay.clone().unwrap();
    bad.entries.push(Entry::Forecast {
        record: ResultRecord {
            input: workload::input(&bad.prehistory, 60_000),
            available_at_ms: 5000,
            snapshot: None,
            error: Some("test".into()),
        },
    });
    assert!(bad.validate().is_err());
    let mut bad = s.replay.clone().unwrap();
    if let Entry::Decision { decision } = &mut bad.entries[0] {
        decision.lane = 3;
    }
    assert!(bad.validate().is_err());
}
struct FailedJudge;
impl Evaluator for FailedJudge {
    fn evaluate(&self, _: Evidence) -> judge::Evaluation<'_> {
        Box::pin(async { Inference::failed("test timeout") })
    }
}
#[tokio::test]
async fn evaluation_errors_take_explicit_fallback_and_replay() {
    let mut s = Session::new(
        42,
        Options {
            recording: None,
            forecaster: Some(Arc::new(FakeForecast(Arc::new(AtomicUsize::new(0))))),
            evaluator: Some(Arc::new(FailedJudge)),
        },
        JevSettings::default(),
    )
    .unwrap();
    for _ in 0..20 {
        s.command(Command::Step).await.unwrap();
        tokio::task::yield_now().await;
    }
    assert!(s
        .decisions
        .iter()
        .any(|d| d.result.error.is_some() && !d.applied));
    assert!(s.decisions.iter().any(|d| d.source.contains("Jev error")));
    s.recording.validate().unwrap();
    let expected = serde_json::to_value(s.engines[2].data()).unwrap();
    s.command(Command::Replay).await.unwrap();
    for _ in 0..20 {
        s.command(Command::Step).await.unwrap();
    }
    assert_eq!(expected, serde_json::to_value(s.engines[2].data()).unwrap());
}
struct SlowForecast {
    calls: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Notify>,
}
impl Forecaster for SlowForecast {
    fn description(&self) -> String {
        "slow test fixture".into()
    }
    fn forecast(&self, input: Input) -> ForecastFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.release.notified().await;
            Ok(snapshot(&input))
        })
    }
}
#[tokio::test]
async fn slow_forecast_never_blocks_traffic_or_dispatches_jev_with_stale_evidence() {
    let calls = Arc::new(AtomicUsize::new(0));
    let judges = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let mut s = Session::new(
        42,
        Options {
            recording: None,
            forecaster: Some(Arc::new(SlowForecast {
                calls: calls.clone(),
                release: release.clone(),
            })),
            evaluator: Some(Arc::new(FakeJudge(judges.clone()))),
        },
        JevSettings::default(),
    )
    .unwrap();
    for _ in 0..40 {
        s.command(Command::Step).await.unwrap();
        tokio::task::yield_now().await;
    }
    assert_eq!(s.at(), 40_000);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(s.engines[0].data().offered > 0);
    release.notify_one();
    tokio::task::yield_now().await;
    s.command(Command::Step).await.unwrap();
    assert!(!s.latest.as_ref().unwrap().fresh(s.at()));
    assert_eq!(judges.load(Ordering::SeqCst), 0);
    s.recording.validate().unwrap();
}
#[tokio::test]
#[ignore = "Requires TYPESAFE_API_KEY; sends one synthetic capacity evaluation to Jev"]
async fn live_jev_accepts_capacity_evidence() {
    let key = std::env::var("TYPESAFE_API_KEY").expect("TYPESAFE_API_KEY");
    let client = typesafe_ai::TypeSafeClient::builder()
        .api_key(key)
        .timeout(Duration::from_secs(2))
        .max_retries(0)
        .build()
        .unwrap();
    let judge = judge::LiveEvaluator::new(client, "jev-1.13.0".into());
    let e = Engine::new(Settings::default()).unwrap();
    let h = workload::prehistory(42, Scenario::Cycles, &workload::defaults());
    let f = snapshot(&workload::input(&h, 0));
    let evidence = judge::evidence(&e.data(), &h, Some(&f));
    let allowed = evidence.legal_choices.clone();
    let r = judge.evaluate(evidence).await;
    assert!(r.error.is_none(), "{:?}", r.error);
    assert!(allowed.contains(&r.choice.unwrap()));
    println!(
        "Live Jev capacity judgment: {:?}; confidence {:?}; model {:?}",
        r.choice, r.confidence, r.model
    );
}

#[tokio::test]
async fn a_single_legal_action_skips_jev_and_replays_without_cost() {
    let judges = Arc::new(AtomicUsize::new(0));
    let mut s = Session::new(
        42,
        Options {
            recording: None,
            forecaster: Some(Arc::new(FakeForecast(Arc::new(AtomicUsize::new(0))))),
            evaluator: Some(Arc::new(FakeJudge(judges.clone()))),
        },
        JevSettings::default(),
    )
    .unwrap();
    s.command(Command::Settings {
        settings: Settings {
            startup_s: 5,
            max_nodes: 2,
        },
    })
    .await
    .unwrap();
    let mut c = s.clients[0].clone();
    c.rate = 4.;
    c.cpu = 8;
    c.memory_gib = 16;
    s.command(Command::Client { client: c }).await.unwrap();
    for _ in 0..20 {
        s.command(Command::Step).await.unwrap();
        tokio::task::yield_now().await;
    }
    assert_eq!(judges.load(Ordering::SeqCst), 0);
    assert_eq!(s.cost.lock().unwrap().calls, 0);
    let d = s
        .decisions
        .iter()
        .find(|d| d.source == "deterministic · only legal action")
        .unwrap();
    assert_eq!(d.evidence.legal_choices, vec![Action::Hold]);
    assert!(d.applied);
    assert!(d.result.confidence.is_none());
    let expected = serde_json::to_value(s.engines[2].data()).unwrap();
    s.command(Command::Replay).await.unwrap();
    for _ in 0..20 {
        s.command(Command::Step).await.unwrap();
    }
    assert_eq!(expected, serde_json::to_value(s.engines[2].data()).unwrap());
}
