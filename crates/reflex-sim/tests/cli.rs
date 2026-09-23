// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

use reflex_sim::{
    presets,
    report::{write_report, Report, ScenarioReport},
};
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

struct OutputDirectory(PathBuf);
impl OutputDirectory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        Self(std::env::temp_dir().join(format!(
            "reflex-sim-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for OutputDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_reflex-sim"))
}

#[test]
fn cli_runs_custom_scenario_and_writes_complete_standalone_report() {
    let directory = OutputDirectory::new();
    let result = cli()
        .args(["--no-open", "--scenario-file"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scenarios/slowdown.json"
        ))
        .args([
            "--seed",
            "7",
            "--algorithms",
            "threshold,unprotected",
            "--output",
        ])
        .arg(&directory.0)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.0.join("results.json")).unwrap()).unwrap();
    assert_eq!(data["seed"], 7);
    assert_eq!(data["scenarios"].as_array().unwrap().len(), 1);
    let runs = data["scenarios"][0]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0]["algorithm"], "threshold");
    assert_eq!(runs[1]["algorithm"], "unprotected");
    let offered = data["scenarios"][0]["offered_requests"].as_u64().unwrap();
    assert!(offered > 0);
    for run in runs {
        assert_eq!(run["requests"].as_array().unwrap().len() as u64, offered);
    }
    let html = std::fs::read_to_string(directory.0.join("index.html")).unwrap();
    assert!(html.contains("Reflex · Circuit Lab"));
    assert!(!html.contains("__REPORT_DATA__"));
    assert!(!html.contains("/*REPORT_JS*/"));
    assert!(!html.contains("<script src="));
    assert!(!html.contains("<link rel=\"stylesheet\""));
}

#[test]
fn invalid_cli_inputs_fail_without_writing_reports() {
    for args in [
        vec!["--scenario", "missing"],
        vec!["--algorithms", "jev"],
        vec!["--algorithms", "threshold,threshold"],
        vec!["--scenario", "healthy", "--scenario-file", "unused.json"],
    ] {
        let directory = OutputDirectory::new();
        let result = cli()
            .args(args)
            .args(["--no-open", "--output"])
            .arg(&directory.0)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!directory.0.exists());
    }
}

#[test]
fn report_escapes_embedded_json_without_changing_exported_content() {
    let directory = OutputDirectory::new();
    let mut scenario = presets().remove(0);
    scenario.name = "</script><script>alert('example')</script>&\u{2028}\u{2029}".into();
    let report = Report {
        version: "test".into(),
        seed: 42,
        scenarios: vec![ScenarioReport {
            scenario,
            trace_fingerprint: "fixture".into(),
            offered_requests: 0,
            runs: vec![],
        }],
    };
    let path = write_report(&report, &directory.0).unwrap();
    let html = std::fs::read_to_string(path).unwrap();
    let json = html
        .split("<script id=\"report-data\" type=\"application/json\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    assert!(!json.contains('<'));
    assert!(!json.contains('&'));
    assert!(!html.contains("<script>alert"));
    let embedded: serde_json::Value = serde_json::from_str(json).unwrap();
    let exported: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.0.join("results.json")).unwrap()).unwrap();
    assert_eq!(embedded, exported);
    assert_eq!(
        exported["scenarios"][0]["scenario"]["name"],
        report.scenarios[0].scenario.name
    );
}
