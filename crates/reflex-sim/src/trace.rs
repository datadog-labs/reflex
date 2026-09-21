use crate::{scenario::Scenario, Error};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: usize,
    pub at_ms: f64,
    pub service: usize,
    pub work_ms: f64,
    pub error_roll: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub model_version: String,
    pub scenario: String,
    pub seed: u64,
    pub fingerprint: String,
    pub requests: Vec<Request>,
}
pub(crate) struct Random(pub(crate) u64);
impl Random {
    pub(crate) fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^= z >> 31;
        ((z >> 12) as f64 + 0.5) / 4_503_599_627_370_496.0
    }
}
pub fn generate_trace(scenario: &Scenario, seed: u64) -> Result<Trace, Error> {
    scenario.validate()?;
    let mut requests = Vec::new();
    for (index, service) in scenario.services.iter().enumerate() {
        let mut rng = Random(seed ^ (index as u64 + 1).wrapping_mul(0xd1b54a32d192ed03));
        for phase in &scenario.phases {
            let rate = scenario.environment(index, phase.start_ms).rate;
            if rate == 0.0 {
                continue;
            }
            let mut at = phase.start_ms;
            loop {
                at += -rng.next().ln() * 1000.0 / rate;
                if at >= phase.end_ms {
                    break;
                }
                let work = service.work_ms * (0.65 + 0.7 * rng.next());
                requests.push(Request {
                    id: 0,
                    at_ms: at,
                    service: index,
                    work_ms: work,
                    error_roll: rng.next(),
                });
                if requests.len() > 200_000 {
                    return Err(Error::Invalid(
                        "trace exceeds 200,000 requests; lower duration or offered rate".into(),
                    ));
                }
            }
        }
    }
    requests.sort_by(|a, b| a.at_ms.total_cmp(&b.at_ms).then(a.service.cmp(&b.service)));
    for (id, r) in requests.iter_mut().enumerate() {
        r.id = id;
    }
    let bytes = serde_json::to_vec(&("queue-stress-v1", scenario, seed, &requests))?;
    // Stable identity for matching traces, not a cryptographic integrity check.
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    });
    Ok(Trace {
        model_version: "queue-stress-v1".into(),
        scenario: scenario.id.clone(),
        seed,
        fingerprint: format!("{hash:016x}"),
        requests,
    })
}
