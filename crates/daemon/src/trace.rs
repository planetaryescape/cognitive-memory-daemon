//! Per-query trace ring buffer.
//!
//! Bounded in-memory ring (default 1000 entries). Each request handler
//! pushes one `Trace` entry on completion. Diagnostic clients fetch by
//! `trace_id` via `Diagnostics::Trace` (Phase 11+, wiring lands when the
//! request kind is added to the protocol).

use std::collections::VecDeque;
use std::sync::Mutex;

/// Per-stage timings for one request. Stage names are stable across
/// versions — adding a new stage is an additive (Some(..)) field.
#[derive(Debug, Clone)]
pub struct Trace {
    pub trace_id: String,
    pub request_id: u64,
    pub bucket: &'static str,
    pub op: &'static str,
    pub embed_ms: Option<f64>,
    pub vector_ms: Option<f64>,
    pub fusion_ms: Option<f64>,
    pub format_ms: Option<f64>,
    pub elapsed_ms: f64,
}

/// Bounded ring buffer of recent traces. Older traces are evicted as new
/// ones land. Lookup is by `trace_id`; iteration is recent-first.
pub struct TraceRing {
    inner: Mutex<VecDeque<Trace>>,
    capacity: usize,
}

impl TraceRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity.max(1))),
            capacity: capacity.max(1),
        }
    }

    pub fn push(&self, trace: Trace) {
        if let Ok(mut guard) = self.inner.lock() {
            if guard.len() >= self.capacity {
                guard.pop_front();
            }
            guard.push_back(trace);
        }
    }

    pub fn get(&self, trace_id: &str) -> Option<Trace> {
        let guard = self.inner.lock().ok()?;
        guard.iter().rev().find(|t| t.trace_id == trace_id).cloned()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trace(id: &str) -> Trace {
        Trace {
            trace_id: id.to_string(),
            request_id: 0,
            bucket: "Memory",
            op: "Search",
            embed_ms: Some(1.0),
            vector_ms: Some(2.0),
            fusion_ms: None,
            format_ms: Some(0.5),
            elapsed_ms: 3.5,
        }
    }

    #[test]
    fn ring_evicts_oldest_at_capacity() {
        let ring = TraceRing::new(3);
        ring.push(make_trace("a"));
        ring.push(make_trace("b"));
        ring.push(make_trace("c"));
        ring.push(make_trace("d"));

        assert_eq!(ring.len(), 3);
        assert!(ring.get("a").is_none(), "oldest must have been evicted");
        assert!(ring.get("d").is_some());
    }

    #[test]
    fn ring_lookup_returns_recent_match() {
        let ring = TraceRing::new(10);
        ring.push(make_trace("x"));
        let found = ring.get("x").expect("must find x");
        assert_eq!(found.trace_id, "x");
    }

    #[test]
    fn ring_zero_capacity_clamps_to_one() {
        let ring = TraceRing::new(0);
        ring.push(make_trace("a"));
        assert_eq!(ring.len(), 1);
    }
}
