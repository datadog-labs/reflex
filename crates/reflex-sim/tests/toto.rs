// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use axum::{routing::post, Json, Router};
use reflex_sim::{
    capacity::forecast::{Forecaster, Input, Series, Snapshot, EPOCH},
    toto::LocalToto,
};

fn input(interval_ms: u64) -> Input {
    let n = if interval_ms == 1000 { 64 } else { 32 };
    Input {
        origin_ms: n * interval_ms,
        interval_ms,
        timestamps: (1..=n)
            .map(|i| EPOCH + (i * interval_ms / 1000) as i64)
            .collect(),
        values: vec![vec![1., 2., 3.]; n as usize],
        prediction_length: 12,
    }
}
fn snapshot(input: &Input) -> Snapshot {
    Snapshot {
        origin_ms: input.origin_ms,
        interval_ms: input.interval_ms,
        request_id: "test".into(),
        source: "local_toto".into(),
        model_provenance: "fixture@revision".into(),
        quantiles: [0.1, 0.5, 0.9],
        latency_ms: 1.,
        series: vec![
            Series {
                lower: vec![0.; 12],
                median: vec![1.; 12],
                upper: vec![2.; 12]
            };
            3
        ],
    }
}
async fn server(app: Router) -> (LocalToto, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = LocalToto::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (provider, task)
}

#[test]
fn only_loopback_base_urls_are_accepted() {
    for url in [
        "https://example.com",
        "http://192.168.1.10:8765",
        "http://user:secret@localhost",
        "http://localhost/path",
        "http://localhost/?token=test",
        "http://localhost/#fragment",
    ] {
        assert!(LocalToto::new(url).is_err(), "{url}");
    }
    for url in [
        "http://127.0.0.1:8765",
        "http://localhost:8765/",
        "http://[::1]:8765",
    ] {
        assert!(LocalToto::new(url).is_ok(), "{url}");
    }
}

#[tokio::test]
async fn http_roundtrip_preserves_both_time_grids() {
    let (provider, task) = server(Router::new().route(
        "/forecast",
        post(|Json(input): Json<Input>| async move {
            input.validate().unwrap();
            Json(snapshot(&input))
        }),
    ))
    .await;
    for interval in [1000, 10000] {
        let data = input(interval);
        provider
            .forecast(data.clone())
            .await
            .unwrap()
            .validate(&data)
            .unwrap();
    }
    task.abort();
}

#[tokio::test]
async fn rejects_wrong_origin_and_crossing_quantiles() {
    for corrupt in [0, 1, 2] {
        let (provider, task) = server(Router::new().route(
            "/forecast",
            post(move |Json(input): Json<Input>| async move {
                let mut s = snapshot(&input);
                match corrupt {
                    0 => s.origin_ms += 1000,
                    1 => s.series[0].lower[0] = 4.,
                    _ => s.model_provenance.clear(),
                }
                Json(s)
            }),
        ))
        .await;
        assert!(provider.forecast(input(1000)).await.is_err());
        task.abort();
    }
}

#[tokio::test]
async fn handles_http_errors_redirects_and_malformed_responses() {
    use axum::http::StatusCode;
    for (status, body) in [
        (
            StatusCode::TOO_MANY_REQUESTS,
            "private error details".to_owned(),
        ),
        (StatusCode::FOUND, "redirect".into()),
        (StatusCode::OK, "{}".into()),
        (StatusCode::OK, "x".repeat(128 * 1024 + 1)),
    ] {
        let (provider, task) =
            server(Router::new().route("/forecast", post(move || async move { (status, body) })))
                .await;
        let error = provider.forecast(input(1000)).await.unwrap_err();
        assert!(!error.contains("private error details"));
        task.abort();
    }
}

#[tokio::test]
#[ignore = "Requires local reflex-toto service on port 8765; performs real inference"]
async fn live_local_toto_forecasts_both_grids() {
    let provider = LocalToto::new("http://127.0.0.1:8765").unwrap();
    for interval in [1000, 10000] {
        let mut data = input(interval);
        data.prediction_length = if interval == 1000 { 120 } else { 12 };
        let result = provider.forecast(data.clone()).await.unwrap();
        result.validate(&data).unwrap();
        assert!(result.model_provenance.contains("Datadog/Toto-2.0-22m@"));
        eprintln!(
            "interval={interval} latency_ms={:.1} model={}",
            result.latency_ms, result.model_provenance
        );
    }
}
