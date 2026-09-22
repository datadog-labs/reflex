use crate::{engine::Run, scenario::Scenario, Error};
use serde::Serialize;
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioReport {
    pub scenario: Scenario,
    pub trace_fingerprint: String,
    pub offered_requests: usize,
    pub runs: Vec<Run>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub version: String,
    pub seed: u64,
    pub scenarios: Vec<ScenarioReport>,
}
/// A self-contained report: no CDN, server, analytics, or network requests.
pub fn write_report(report: &Report, directory: &Path) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(directory)?;
    let data = serde_json::to_string(report)?;
    std::fs::write(directory.join("results.json"), &data)?;
    let safe = data
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    let html = include_str!("web/report.html")
        .replace("/*REPORT_CSS*/", include_str!("web/report.css"))
        .replace("/*REPORT_JS*/", include_str!("web/report.js"))
        .replace("__REPORT_DATA__", &safe);
    let file = directory.join("index.html");
    std::fs::write(&file, html)?;
    Ok(file.canonicalize()?)
}
pub fn open_report(path: &Path) -> Result<(), Error> {
    open_target(path)
}
pub fn open_target(target: impl AsRef<std::ffi::OsStr>) -> Result<(), Error> {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let status = command.arg(target.as_ref()).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "could not open browser; open {} manually",
            target.as_ref().to_string_lossy()
        )))
    }
}
