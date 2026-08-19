use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct MetricsRegistry {
    counters: RwLock<HashMap<String, AtomicU64>>,
    gauges: RwLock<HashMap<String, f64>>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment(&self, name: &str, delta: u64) {
        let guard = self.counters.read().unwrap();
        if let Some(counter) = guard.get(name) {
            counter.fetch_add(delta, Ordering::Relaxed);
            return;
        }
        drop(guard);

        let mut write_guard = self.counters.write().unwrap();
        write_guard
            .entry(name.to_string())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(delta, Ordering::Relaxed);
    }

    pub fn get(&self, name: &str) -> u64 {
        let guard = self.counters.read().unwrap();
        guard
            .get(name)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn record_gauge(&self, name: &str, value: f64) {
        let mut guard = self.gauges.write().unwrap();
        guard.insert(name.to_string(), value);
    }

    pub fn get_gauge(&self, name: &str) -> f64 {
        let guard = self.gauges.read().unwrap();
        guard.get(name).copied().unwrap_or(0.0)
    }

    pub fn snapshot(&self) -> (HashMap<String, u64>, HashMap<String, f64>) {
        let c_guard = self.counters.read().unwrap();
        let counters: HashMap<String, u64> = c_guard
            .iter()
            .map(|(k, v)| (k.clone(), v.load(Ordering::Relaxed)))
            .collect();

        let g_guard = self.gauges.read().unwrap();
        let gauges: HashMap<String, f64> = g_guard.clone();

        (counters, gauges)
    }
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
    fn test_metrics_registry() {
        let registry = MetricsRegistry::new();
        registry.increment("cells_claimed", 5);
        registry.increment("cells_claimed", 3);
        assert_eq!(registry.get("cells_claimed"), 8);

        registry.record_gauge("cpu_utilization", 0.75);
        assert_eq!(registry.get_gauge("cpu_utilization"), 0.75);

        let (counters, gauges) = registry.snapshot();
        assert_eq!(counters.get("cells_claimed"), Some(&8));
        assert_eq!(gauges.get("cpu_utilization"), Some(&0.75));
    }
}
