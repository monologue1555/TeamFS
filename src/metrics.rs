//! Runtime diagnostics only. Percentiles use explicitly bounded recent samples.
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};

const SAMPLES: usize = 256;
const SLOWEST: usize = 20;
#[derive(Clone, Copy, Default, Serialize)]
pub struct Timing {
    pub total_us: u64,
    pub lock_wait_us: u64,
    pub service_us: u64,
    /// Included in service_us, never add it to service_us again.
    pub commit_us: u64,
}
#[derive(Default)]
struct Operation {
    calls: u64,
    errors: u64,
    total: Timing,
    max_us: u64,
    samples: VecDeque<u64>,
}
#[derive(Clone, Serialize)]
struct Slow {
    sequence: u64,
    operation: String,
    path_bytes: Option<Vec<u8>>,
    path_display: Option<String>,
    pid: Option<u32>,
    errno: Option<i32>,
    timing: Timing,
}
#[derive(Default)]
pub struct Metrics {
    operations: BTreeMap<String, Operation>,
    slowest: Vec<Slow>,
    sequence: u64,
    pub commit_total_us: u64,
}
impl Metrics {
    pub fn record(
        &mut self,
        operation: &str,
        path: Option<&[u8]>,
        pid: Option<u32>,
        error: Option<i32>,
        timing: Timing,
    ) {
        self.sequence += 1;
        let entry = self.operations.entry(operation.into()).or_default();
        entry.calls += 1;
        entry.errors += u64::from(error.is_some());
        entry.total.total_us = entry.total.total_us.saturating_add(timing.total_us);
        entry.total.lock_wait_us = entry.total.lock_wait_us.saturating_add(timing.lock_wait_us);
        entry.total.service_us = entry.total.service_us.saturating_add(timing.service_us);
        entry.total.commit_us = entry.total.commit_us.saturating_add(timing.commit_us);
        entry.max_us = entry.max_us.max(timing.total_us);
        entry.samples.push_back(timing.total_us);
        if entry.samples.len() > SAMPLES {
            entry.samples.pop_front();
        }
        if operation == "sync.commit" {
            self.commit_total_us = self.commit_total_us.saturating_add(timing.total_us);
        }
        let slow = Slow {
            sequence: self.sequence,
            operation: operation.into(),
            path_bytes: path.map(<[u8]>::to_vec),
            path_display: path.map(crate::history::display_path),
            pid,
            errno: error,
            timing,
        };
        if self.slowest.len() < SLOWEST
            || self
                .slowest
                .last()
                .map_or(false, |s| timing.total_us > s.timing.total_us)
        {
            self.slowest.push(slow);
            self.slowest
                .sort_by_key(|s| (std::cmp::Reverse(s.timing.total_us), s.sequence));
            self.slowest.truncate(SLOWEST);
        }
    }
    pub fn summary(&self) -> Value {
        let operations: Vec<_> = self.operations.iter().map(|(name, s)| {
            let mut samples: Vec<_> = s.samples.iter().copied().collect();
            samples.sort_unstable();
            let percentile = |percent: usize| samples[(samples.len()*percent + 99)/100 - 1];
            json!({"operation":name,"calls":s.calls,"errors":s.errors,"total_us":s.total.total_us,
                "mean_us":s.total.total_us as f64/s.calls as f64,"max_us":s.max_us,
                "lock_wait_total_us":s.total.lock_wait_us,"service_total_us":s.total.service_us,
                "commit_total_us":s.total.commit_us,"sample_count":samples.len(),
                "p50_us":percentile(50),"p95_us":percentile(95),"p99_us":percentile(99)})
        }).collect();
        json!({"schema_version":1,"recorded":self.sequence,"sample_limit_per_operation":SAMPLES,
            "percentile_scope":"most recent samples per operation; counters cover this mount",
            "timing_scope":"userspace callback only; excludes kernel queue and reply delivery; commit is included in service",
            "operations":operations,"slowest":self.slowest})
    }
    pub fn bytes(&self) -> Vec<u8> {
        crate::history::json_bytes(&self.summary())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_percentiles_and_lifetime_totals_are_distinct() {
        let mut m = Metrics::default();
        for n in 1..=300 {
            m.record(
                "fuse.write",
                Some(b"a\xff"),
                Some(12),
                if n == 1 { Some(5) } else { None },
                Timing {
                    total_us: n,
                    ..Timing::default()
                },
            );
        }
        let s = m.summary();
        let op = &s["operations"][0];
        assert_eq!(op["calls"], 300);
        assert_eq!(op["errors"], 1);
        assert_eq!(op["sample_count"], 256);
        assert_eq!(op["p50_us"], 172);
        assert_eq!(op["p95_us"], 288);
        assert_eq!(op["max_us"], 300);
        assert_eq!(s["slowest"].as_array().unwrap().len(), 20);
        assert_eq!(s["slowest"][0]["timing"]["total_us"], 300);
        assert_eq!(s["slowest"][0]["path_bytes"], json!([97, 255]));
        assert!(Metrics::default().summary()["operations"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}
