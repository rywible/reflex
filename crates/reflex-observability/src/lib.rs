use std::collections::BTreeMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

pub const CELLS_STARTED_TOTAL: &str = "reflex_cells_started_total";
pub const CELLS_ENQUEUED_TOTAL: &str = "reflex_cells_enqueued_total";
pub const EXPERIMENTS_CREATED_TOTAL: &str = "reflex_experiments_created_total";
pub const ENGINE_STARTS_TOTAL: &str = "reflex_engine_starts_total";
pub const ENGINE_UP: &str = "reflex_engine_up";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetricKind {
    Counter,
    Gauge,
}

#[derive(Clone, Copy, Debug)]
struct MetricDescriptor {
    name: &'static str,
    unit: &'static str,
    help: &'static str,
    kind: MetricKind,
}

const METRICS: &[MetricDescriptor] = &[
    MetricDescriptor {
        name: CELLS_STARTED_TOTAL,
        unit: "cells",
        help: "Cells started by the local engine.",
        kind: MetricKind::Counter,
    },
    MetricDescriptor {
        name: CELLS_ENQUEUED_TOTAL,
        unit: "cells",
        help: "Cells enqueued by the local engine.",
        kind: MetricKind::Counter,
    },
    MetricDescriptor {
        name: EXPERIMENTS_CREATED_TOTAL,
        unit: "experiments",
        help: "Experiments created by the local engine.",
        kind: MetricKind::Counter,
    },
    MetricDescriptor {
        name: ENGINE_STARTS_TOTAL,
        unit: "starts",
        help: "Successful local engine initializations.",
        kind: MetricKind::Counter,
    },
    MetricDescriptor {
        name: ENGINE_UP,
        unit: "state",
        help: "Whether the local engine is running.",
        kind: MetricKind::Gauge,
    },
];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MetricsError {
    #[error("metric `{0}` is not in the bounded metric registry")]
    UnknownMetric(String),
    #[error("metric `{name}` is a {actual}, not a {requested}")]
    WrongKind {
        name: String,
        requested: &'static str,
        actual: &'static str,
    },
    #[error("gauge `{0}` rejected a non-finite value")]
    NonFiniteGauge(String),
    #[error("metrics registry lock poisoned")]
    LockPoisoned,
}

/// Fixed-cardinality metrics. There is deliberately no label API: request,
/// experiment, cell, state, and candidate IDs must not become dimensions.
#[derive(Default)]
pub struct MetricsRegistry {
    counters: RwLock<BTreeMap<&'static str, u64>>,
    gauges: RwLock<BTreeMap<&'static str, f64>>,
}

pub type MetricsSnapshot = (BTreeMap<&'static str, u64>, BTreeMap<&'static str, f64>);

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            counters: RwLock::new(
                METRICS
                    .iter()
                    .filter(|m| m.kind == MetricKind::Counter)
                    .map(|m| (m.name, 0))
                    .collect(),
            ),
            gauges: RwLock::new(
                METRICS
                    .iter()
                    .filter(|m| m.kind == MetricKind::Gauge)
                    .map(|m| (m.name, 0.0))
                    .collect(),
            ),
        }
    }

    pub fn increment(&self, name: &str, delta: u64) -> Result<(), MetricsError> {
        let metric = descriptor(name)?;
        require_kind(metric, MetricKind::Counter)?;
        let mut values = self
            .counters
            .write()
            .map_err(|_| MetricsError::LockPoisoned)?;
        let value = values
            .get_mut(metric.name)
            .ok_or_else(|| MetricsError::UnknownMetric(name.into()))?;
        *value = value.saturating_add(delta);
        Ok(())
    }

    pub fn record_gauge(&self, name: &str, value: f64) -> Result<(), MetricsError> {
        if !value.is_finite() {
            return Err(MetricsError::NonFiniteGauge(name.into()));
        }
        let metric = descriptor(name)?;
        require_kind(metric, MetricKind::Gauge)?;
        *self
            .gauges
            .write()
            .map_err(|_| MetricsError::LockPoisoned)?
            .get_mut(metric.name)
            .ok_or_else(|| MetricsError::UnknownMetric(name.into()))? = value;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<u64, MetricsError> {
        let metric = descriptor(name)?;
        require_kind(metric, MetricKind::Counter)?;
        self.counters
            .read()
            .map_err(|_| MetricsError::LockPoisoned)?
            .get(metric.name)
            .copied()
            .ok_or_else(|| MetricsError::UnknownMetric(name.into()))
    }

    pub fn snapshot(&self) -> Result<MetricsSnapshot, MetricsError> {
        Ok((
            self.counters
                .read()
                .map_err(|_| MetricsError::LockPoisoned)?
                .clone(),
            self.gauges
                .read()
                .map_err(|_| MetricsError::LockPoisoned)?
                .clone(),
        ))
    }

    pub fn to_prometheus_text(&self) -> Result<String, MetricsError> {
        let (counters, gauges) = self.snapshot()?;
        let mut out = String::new();
        for metric in METRICS {
            out.push_str(&format!(
                "# HELP {} {}\n# TYPE {} {}\n# UNIT {} {}\n",
                metric.name,
                metric.help,
                metric.name,
                kind_name(metric.kind),
                metric.name,
                metric.unit
            ));
            let value = match metric.kind {
                MetricKind::Counter => counters.get(metric.name).map(u64::to_string),
                MetricKind::Gauge => gauges.get(metric.name).map(f64::to_string),
            }
            .ok_or_else(|| MetricsError::UnknownMetric(metric.name.into()))?;
            out.push_str(&format!("{} {}\n", metric.name, value));
        }
        Ok(out)
    }
}

fn descriptor(name: &str) -> Result<&'static MetricDescriptor, MetricsError> {
    METRICS
        .iter()
        .find(|metric| metric.name == name)
        .ok_or_else(|| MetricsError::UnknownMetric(name.into()))
}

fn kind_name(kind: MetricKind) -> &'static str {
    match kind {
        MetricKind::Counter => "counter",
        MetricKind::Gauge => "gauge",
    }
}

fn require_kind(metric: &MetricDescriptor, requested: MetricKind) -> Result<(), MetricsError> {
    if metric.kind == requested {
        return Ok(());
    }
    Err(MetricsError::WrongKind {
        name: metric.name.into(),
        requested: kind_name(requested),
        actual: kind_name(metric.kind),
    })
}

/// Redact credential-shaped assignments everywhere in a bounded log string.
pub fn redact_secrets(input: &str) -> String {
    const MAX_LOG_BYTES: usize = 16 * 1024;
    let bounded = if input.len() > MAX_LOG_BYTES {
        &input[..input.floor_char_boundary(MAX_LOG_BYTES)]
    } else {
        input
    };
    let bearer_redacted = redact_bearer_credentials(bounded);
    let mut output = bearer_redacted
        .split_inclusive(char::is_whitespace)
        .map(redact_word)
        .collect::<String>();
    if input.len() > bounded.len() {
        output.push_str("[TRUNCATED]");
    }
    output
}

fn redact_bearer_credentials(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut copied_through = 0;
    let mut search_from = 0;
    while let Some((value_start, value_end)) = find_bearer_value(input, search_from) {
        output.push_str(&input[copied_through..value_start]);
        output.push_str("[REDACTED]");
        copied_through = value_end;
        search_from = value_end;
    }
    output.push_str(&input[copied_through..]);
    output
}

fn find_bearer_value(input: &str, from: usize) -> Option<(usize, usize)> {
    for (relative, _) in input[from..].char_indices() {
        let start = from + relative;
        let end = start.checked_add(6)?;
        if end > input.len()
            || !input.is_char_boundary(end)
            || !input[start..end].eq_ignore_ascii_case("bearer")
        {
            continue;
        }
        let before_is_word = input[..start]
            .chars()
            .next_back()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_');
        if before_is_word {
            continue;
        }
        let mut value_start = end;
        let mut saw_separator = false;
        for (index, character) in input[end..].char_indices() {
            if character.is_ascii_whitespace() {
                saw_separator = true;
                value_start = end + index + character.len_utf8();
            } else {
                break;
            }
        }
        if !saw_separator || value_start >= input.len() {
            continue;
        }
        let value_end = input[value_start..]
            .char_indices()
            .find(|(_, character)| {
                character.is_ascii_whitespace()
                    || matches!(character, '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}')
            })
            .map_or(input.len(), |(index, _)| value_start + index);
        if value_end > value_start {
            return Some((value_start, value_end));
        }
    }
    None
}

fn redact_word(word: &str) -> String {
    for separator in ['=', ':'] {
        let Some(index) = word.find(separator) else {
            continue;
        };
        if word[index + 1..].trim().is_empty() {
            continue;
        }
        let key = word[..index]
            .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .to_ascii_lowercase();
        if key.ends_with("token")
            || key.ends_with("secret")
            || key.ends_with("password")
            || key.ends_with("api_key")
            || key == "authorization"
        {
            return replace_secret_value(word, index + 1);
        }
    }
    word.into()
}

fn replace_secret_value(word: &str, start: usize) -> String {
    let end = word[start..]
        .find([',', ';', ')', ']', '}'])
        .map_or(word.len(), |i| start + i);
    format!("{}[REDACTED]{}", &word[..start], &word[end..])
}

static TRACE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Opaque process-local correlation ID; a supplied cell ID is hashed, not exposed.
pub fn new_trace_id(cell_id: Option<&str>) -> String {
    let sequence = TRACE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let material = format!(
        "{}:{sequence}:{}",
        std::process::id(),
        cell_id.unwrap_or("")
    );
    let hex = reflex_types::Digest::hash_blake3(material.as_bytes()).to_hex();
    format!("trace-{}", &hex[..32])
}

#[derive(Clone, Debug, Default)]
pub struct ObservabilityConfig {
    pub otel_enabled: bool,
    pub prometheus_enabled: bool,
    pub sample_rate: f32,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ExporterError {
    #[error("sample rate must be finite and in the inclusive range 0..=1")]
    InvalidSampleRate,
    #[error("OpenTelemetry export was requested but no OTLP transport is configured")]
    TransportUnavailable,
}

/// Disabled export is a no-op. Enabled export fails at construction until an
/// actual OTLP transport is supplied; a tracing log is never called OTLP.
#[derive(Debug)]
pub struct OtelExporter;

impl OtelExporter {
    pub fn new(config: ObservabilityConfig) -> Result<Self, ExporterError> {
        if !config.sample_rate.is_finite() || !(0.0..=1.0).contains(&config.sample_rate) {
            return Err(ExporterError::InvalidSampleRate);
        }
        if config.otel_enabled {
            return Err(ExporterError::TransportUnavailable);
        }
        Ok(Self)
    }
    pub fn export_span(&self, _trace_id: &str, _name: &str) {}
}

pub fn init_subscriber() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_bounded_and_valid() {
        let registry = MetricsRegistry::new();
        registry.increment(CELLS_STARTED_TOTAL, 8).unwrap();
        registry.record_gauge(ENGINE_UP, 1.0).unwrap();
        assert_eq!(registry.get(CELLS_STARTED_TOTAL).unwrap(), 8);
        assert!(registry.increment("candidate_123", 1).is_err());
        assert!(registry.record_gauge(ENGINE_UP, f64::NAN).is_err());
        let text = registry.to_prometheus_text().unwrap();
        assert!(text.contains("# TYPE reflex_cells_started_total counter"));
        assert!(text.contains("reflex_engine_up 1"));
    }

    #[test]
    fn redaction_is_repeated_and_bounded() {
        let redacted = redact_secrets(
            "token=first REFLEX_WORKER_TOKEN=second authorization:third password=fourth Authorization: Bearer fifth next",
        );
        for secret in ["first", "second", "third", "fourth", "fifth"] {
            assert!(!redacted.contains(secret));
        }
        assert_eq!(redacted.matches("[REDACTED]").count(), 5);
        assert!(redact_secrets(&"x".repeat(20_000)).ends_with("[TRUNCATED]"));
    }

    #[test]
    fn bearer_redaction_handles_case_spacing_quotes_and_repetition() {
        let redacted = redact_secrets(
            "{\"authorization\":\"bEaReR\tfirst.jwt\"} Bearer    second, tail prebearer keep",
        );
        assert!(!redacted.contains("first.jwt"));
        assert!(!redacted.contains("second"));
        assert!(redacted.contains("prebearer keep"));
        assert!(redacted.matches("[REDACTED]").count() >= 2);
    }

    #[test]
    fn trace_id_is_opaque() {
        let id = new_trace_id(Some("cell-abc"));
        assert!(id.starts_with("trace-"));
        assert!(!id.contains("cell-abc"));
        assert_ne!(id, new_trace_id(Some("cell-abc")));
    }

    #[test]
    fn fake_enabled_export_is_rejected() {
        OtelExporter::new(ObservabilityConfig::default())
            .unwrap()
            .export_span("t", "s");
        assert_eq!(
            OtelExporter::new(ObservabilityConfig {
                otel_enabled: true,
                sample_rate: 1.0,
                ..Default::default()
            })
            .unwrap_err(),
            ExporterError::TransportUnavailable
        );
    }
}
