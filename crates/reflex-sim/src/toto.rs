// Unless explicitly stated otherwise all files in this repository are licensed under the Apache-2.0 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-present Datadog, Inc.

//! HTTP adapter for the optional local Python Toto service.
use crate::capacity::forecast::{ForecastFuture, Forecaster, Input, Snapshot};
use std::time::Duration;

pub struct LocalToto {
    client: reqwest::Client,
    endpoint: reqwest::Url,
}

impl LocalToto {
    /// Only loopback HTTP is supported: observation data stays on this machine.
    pub fn new(base_url: &str) -> Result<Self, String> {
        let mut endpoint = reqwest::Url::parse(base_url)
            .map_err(|_| "--toto-url must be a loopback HTTP URL".to_owned())?;
        if endpoint.scheme() != "http"
            || !matches!(
                endpoint.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]")
            )
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/"
        {
            return Err("--toto-url must be a loopback HTTP base URL without credentials, path, query, or fragment".into());
        }
        endpoint.set_path("/forecast");
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "Could not construct the local Toto HTTP client".to_owned())?;
        Ok(Self { client, endpoint })
    }
}

impl Forecaster for LocalToto {
    fn minimum_samples(&self, interval_ms: u64) -> usize {
        if interval_ms == 1000 {
            64
        } else {
            32
        }
    }
    fn forecast(&self, input: Input) -> ForecastFuture<'_> {
        Box::pin(async move {
            input.validate()?;
            if input.values.len() < self.minimum_samples(input.interval_ms) {
                return Err(
                    "Local Toto needs at least 32 observed samples (64 on the one-second grid)"
                        .into(),
                );
            }
            let mut response = self
                .client
                .post(self.endpoint.clone())
                .json(&input)
                .send()
                .await
                .map_err(|_| "Local Toto service unavailable or timed out".to_owned())?;
            if !response.status().is_success() {
                // Do not reflect response bodies (which might contain observations) into the UI.
                return Err(format!(
                    "Local Toto returned HTTP {}",
                    response.status().as_u16()
                ));
            }
            const MAX_RESPONSE: usize = 128 * 1024;
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "Could not read the local Toto response".to_owned())?
            {
                if body.len() + chunk.len() > MAX_RESPONSE {
                    return Err("Local Toto response exceeded size limit".into());
                }
                body.extend_from_slice(&chunk);
            }
            let snapshot: Snapshot = serde_json::from_slice(&body)
                .map_err(|_| "Invalid local Toto response".to_owned())?;
            snapshot.validate(&input)?;
            if snapshot.source != "local_toto"
                || snapshot.model_provenance.trim().is_empty()
                || snapshot.request_id.trim().is_empty()
            {
                return Err("Missing local Toto provenance".into());
            }
            Ok(snapshot)
        })
    }

    fn description(&self) -> String {
        "Local open-source Toto".into()
    }
}
