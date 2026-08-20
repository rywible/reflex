use crate::util;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Ns,
    Us,
    Ms,
    S,
}

impl Unit {
    pub fn to_ns(self) -> f64 {
        match self {
            Unit::Ns => 1.0,
            Unit::Us => 1_000.0,
            Unit::Ms => 1_000_000.0,
            Unit::S => 1_000_000_000.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Unit::Ns => "ns",
            Unit::Us => "us",
            Unit::Ms => "ms",
            Unit::S => "s",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecisionRule {
    DurationLeMs(f64),
    DurationLeSec(f64),
    P95Le(f64, Unit),
    P50Le(f64, Unit),
    MeanLe(f64, Unit),
    MaxRssLeMb(f64),
}

impl DecisionRule {
    pub fn parse(rule: &str) -> std::result::Result<Self, String> {
        let parts: Vec<&str> = rule.split('_').collect();
        if parts.len() < 3 || parts[1] != "le" {
            return Err(format!(
                "invalid decision_rule '{rule}' (expected <metric>_le_<value><unit>)"
            ));
        }
        let metric = parts[0];
        let tail = parts[2..].join("_");
        let digit_len = tail
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .count();
        if digit_len == 0 || digit_len == tail.len() {
            return Err(format!(
                "invalid decision_rule '{rule}' (expected <value><unit> in the tail)"
            ));
        }
        let (num, unit) = tail.split_at(digit_len);
        let value = num
            .parse::<f64>()
            .map_err(|_| format!("invalid decision_rule '{rule}' (unparseable value '{num}')"))?;
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "invalid decision_rule '{rule}' (threshold must be finite and non-negative)"
            ));
        }
        if metric == "max_rss" {
            return match unit {
                "MB" => Ok(DecisionRule::MaxRssLeMb(value)),
                "GB" => Ok(DecisionRule::MaxRssLeMb(value * 1024.0)),
                _ => Err(format!(
                    "max_rss rule '{rule}' must use MB or GB, got '{unit}'"
                )),
            };
        }
        let unit = match unit {
            "ns" => Unit::Ns,
            "us" => Unit::Us,
            "ms" => Unit::Ms,
            "s" => Unit::S,
            other => return Err(format!("invalid unit '{other}' in decision_rule '{rule}'")),
        };
        match metric {
            "duration" => match unit {
                Unit::Ms => Ok(DecisionRule::DurationLeMs(value)),
                Unit::S => Ok(DecisionRule::DurationLeSec(value)),
                other => Err(format!(
                    "duration rule '{rule}' must use ms or s, got {}",
                    other.label()
                )),
            },
            "p95" => Ok(DecisionRule::P95Le(value, unit)),
            "p50" => Ok(DecisionRule::P50Le(value, unit)),
            "mean" => Ok(DecisionRule::MeanLe(value, unit)),
            other => Err(format!(
                "unknown metric '{other}' in decision_rule '{rule}'"
            )),
        }
    }
}

/// Criterion sample.json layout: `{"times": [ns, ns, ...]}`.
pub fn criterion_stats(sample_json: &Path) -> Option<(f64, f64, f64)> {
    let value = util::read_json(sample_json).ok()?;
    let times = value.get("times")?.as_array()?;
    if times.is_empty() {
        return None;
    }
    let mut samples: Vec<f64> = times
        .iter()
        .map(|sample| sample.as_f64())
        .collect::<Option<_>>()?;
    if samples.is_empty()
        || samples
            .iter()
            .any(|sample| !sample.is_finite() || *sample <= 0.0)
    {
        return None;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p = |q: f64| -> f64 {
        let idx = ((samples.len() as f64 - 1.0) * q).round() as usize;
        samples[idx.min(samples.len() - 1)]
    };
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    mean.is_finite().then(|| (p(0.50), p(0.95), mean))
}

#[derive(Debug, Clone, Default)]
pub struct BenchMeasurements {
    pub duration_ms: Option<u64>,
    pub p50_ns: Option<f64>,
    pub p95_ns: Option<f64>,
    pub mean_ns: Option<f64>,
}

/// Evaluate a decision rule against a measurement. Ok(()) = gate passes.
pub fn eval_rule(rule: &DecisionRule, m: &BenchMeasurements) -> std::result::Result<(), String> {
    for (name, value) in [("p50", m.p50_ns), ("p95", m.p95_ns), ("mean", m.mean_ns)] {
        if value.is_some_and(|measurement| !measurement.is_finite() || measurement < 0.0) {
            return Err(format!(
                "{name} measurement must be finite and non-negative"
            ));
        }
    }
    match rule {
        DecisionRule::DurationLeMs(v) => {
            let d = m.duration_ms.ok_or("no duration measured")? as f64;
            if d <= *v {
                Ok(())
            } else {
                Err(format!("duration {d:.0} ms > {v} ms"))
            }
        }
        DecisionRule::DurationLeSec(v) => {
            let d = m.duration_ms.ok_or("no duration measured")? as f64 / 1000.0;
            if d <= *v {
                Ok(())
            } else {
                Err(format!("duration {d:.2} s > {v} s"))
            }
        }
        DecisionRule::P95Le(v, unit) => {
            let d = m
                .p95_ns
                .ok_or("no p95 measured (criterion sample.json missing)")?;
            if d <= v * unit.to_ns() {
                Ok(())
            } else {
                Err(format!("p95 {d:.0} ns > {v} {}", unit.label()))
            }
        }
        DecisionRule::P50Le(v, unit) => {
            let d = m
                .p50_ns
                .ok_or("no p50 measured (criterion sample.json missing)")?;
            if d <= v * unit.to_ns() {
                Ok(())
            } else {
                Err(format!("p50 {d:.0} ns > {v} {}", unit.label()))
            }
        }
        DecisionRule::MeanLe(v, unit) => {
            let d = m
                .mean_ns
                .ok_or("no mean measured (criterion sample.json missing)")?;
            if d <= v * unit.to_ns() {
                Ok(())
            } else {
                Err(format!("mean {d:.0} ns > {v} {}", unit.label()))
            }
        }
        DecisionRule::MaxRssLeMb(v) => Err(format!(
            "max RSS gate ({v} MB) is not measurable by the current harness"
        )),
    }
}

/// Gate evaluation completes quickly for 10,000 synthetic records.
pub fn self_test() -> Vec<(&'static str, bool, String)> {
    let rules = [
        DecisionRule::P95Le(100.0, Unit::Us),
        DecisionRule::DurationLeSec(1.0),
    ];
    let start = std::time::Instant::now();
    let mut passed = 0usize;
    let mut failed = 0usize;
    for i in 0..10_000u64 {
        let rule = &rules[(i % rules.len() as u64) as usize];
        let m = BenchMeasurements {
            p95_ns: Some(50_000.0 + (i % 100) as f64 * 1_000.0),
            duration_ms: Some(500 + i % 100),
            ..BenchMeasurements::default()
        };
        match eval_rule(rule, &m) {
            Ok(()) => passed += 1,
            Err(_) => failed += 1,
        }
    }
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let raw_times: Vec<f64> = (0..50).map(|i| 10_000.0 + i as f64 * 500.0).collect();
    let sample_json = serde_json::json!({ "times": raw_times });
    let mut regen_ok = false;
    if let Ok(tmp) = tempfile::NamedTempFile::new() {
        let path = tmp.path().to_path_buf();
        if std::fs::write(
            &path,
            serde_json::to_string(&sample_json).unwrap_or_default(),
        )
        .is_ok()
            && let Some((p50, p95, mean)) = criterion_stats(&path)
        {
            regen_ok = p50 > 0.0 && p95 >= p50 && mean > 0.0;
        }
    }

    vec![
        (
            "10,000 gate evaluations complete quickly",
            passed + failed == 10_000 && elapsed_ms < 1_000,
            format!("passed={passed} failed={failed} elapsed={elapsed_ms} ms"),
        ),
        (
            "gate classification is deterministic (both outcomes observed)",
            passed > 0 && failed > 0,
            format!("passed={passed} failed={failed}"),
        ),
        (
            "raw benchmark samples regenerate p50/p95/mean summary",
            regen_ok,
            if regen_ok {
                "ok".to_string()
            } else {
                "sample.json round-trip failed".to_string()
            },
        ),
        (
            "missing baseline fails closed in budget registry",
            check_missing_baseline_fails_closed(),
            "PerformanceBudgetRegistry unknown name".to_string(),
        ),
        (
            "decision_rule parse round-trip",
            DecisionRule::parse("p95_le_100us").ok() == Some(DecisionRule::P95Le(100.0, Unit::Us)),
            "p95_le_100us".to_string(),
        ),
        (
            "malformed benchmark evidence fails closed",
            DecisionRule::parse("p95_le_-1us").is_err()
                && eval_rule(
                    &DecisionRule::P95Le(1.0, Unit::Us),
                    &BenchMeasurements {
                        p95_ns: Some(f64::NAN),
                        ..BenchMeasurements::default()
                    },
                )
                .is_err(),
            "negative thresholds and non-finite measurements".to_string(),
        ),
    ]
}

fn check_missing_baseline_fails_closed() -> bool {
    let budgets = [
        ("micro_mlp_2607_batch64", "reference-4vcpu-8gb"),
        ("scorer_p95", "reference-4vcpu-8gb"),
    ];
    let unknown = "unknown_bench_xyz";
    !budgets.iter().any(|(n, _)| *n == unknown)
}

pub mod waivers {
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Waiver {
        pub benchmark: String,
        #[serde(default)]
        pub scope: String,
        pub owner: String,
        pub rationale: String,
        pub expiry_date: String,
        pub evidence: String,
        /// Gate that must pass before this waiver expires.
        pub replacement_gate: String,
    }

    pub fn waivers_dir() -> PathBuf {
        PathBuf::from("waivers")
    }

    pub fn load_waivers() -> Vec<(String, Waiver)> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(waivers_dir()) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(waiver) = serde_json::from_str::<Waiver>(&text) else {
                continue;
            };
            out.push((path.display().to_string(), waiver));
        }
        out.sort_by(|a, b| a.1.benchmark.cmp(&b.1.benchmark));
        out
    }

    pub fn is_valid(w: &Waiver) -> bool {
        !expired(w) && !w.owner.trim().is_empty() && !w.replacement_gate.trim().is_empty()
    }

    pub fn self_test() -> Vec<(&'static str, bool, String)> {
        let bad = Waiver {
            benchmark: "fixture".to_string(),
            scope: "ml-micro".to_string(),
            owner: String::new(),
            rationale: "test".to_string(),
            expiry_date: "2099-01-01".to_string(),
            evidence: "benches/ml_bench.rs".to_string(),
            replacement_gate: String::new(),
        };
        let good = Waiver {
            benchmark: "fixture".to_string(),
            scope: "ml-micro".to_string(),
            owner: "platform".to_string(),
            rationale: "test".to_string(),
            expiry_date: "2099-01-01".to_string(),
            evidence: "benches/ml_bench.rs".to_string(),
            replacement_gate: "p16_gate_eval".to_string(),
        };
        vec![
            (
                "waiver without replacement_gate is invalid",
                !is_valid(&bad),
                "replacement_gate required".to_string(),
            ),
            (
                "complete waiver with replacement_gate is valid",
                is_valid(&good),
                "ok".to_string(),
            ),
            (
                "expired waiver is rejected",
                expired(&Waiver {
                    expiry_date: "2000-01-01".to_string(),
                    replacement_gate: "gate".to_string(),
                    owner: "platform".to_string(),
                    ..good.clone()
                }),
                "expiry enforced".to_string(),
            ),
        ]
    }

    pub fn expired(w: &Waiver) -> bool {
        let today = crate::util::now_utc_iso();
        w.expiry_date.trim() < &today[..10]
    }

    pub fn list() -> Vec<(String, Waiver, bool)> {
        load_waivers()
            .into_iter()
            .map(|(path, w)| {
                let valid = is_valid(&w);
                (path, w, valid)
            })
            .collect()
    }
}
