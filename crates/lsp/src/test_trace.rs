//! Tiny in-process tracing subscriber for assertions about span fields.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

#[derive(Clone, Debug)]
pub(crate) struct CapturedSpan {
    pub(crate) name: &'static str,
    fields: HashMap<&'static str, String>,
}

impl CapturedSpan {
    pub(crate) fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

#[derive(Debug)]
pub(crate) struct CapturedTrace {
    spans: Vec<CapturedSpan>,
}

impl CapturedTrace {
    pub(crate) fn spans_named(&self, name: &str) -> Vec<&CapturedSpan> {
        self.spans.iter().filter(|span| span.name == name).collect()
    }

    pub(crate) fn only_span(&self, name: &str) -> &CapturedSpan {
        let spans = self.spans_named(name);
        assert_eq!(
            spans.len(),
            1,
            "expected one {name:?} span: {:#?}",
            self.spans
        );
        spans[0]
    }
}

#[derive(Clone)]
struct CaptureSubscriber {
    shared: Arc<Shared>,
}

struct Shared {
    next_id: AtomicU64,
    spans: Mutex<Vec<CapturedSpan>>,
}

impl CaptureSubscriber {
    fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                next_id: AtomicU64::new(1),
                spans: Mutex::new(Vec::new()),
            }),
        }
    }

    fn trace(&self) -> CapturedTrace {
        CapturedTrace {
            spans: self.shared.spans.lock().unwrap().clone(),
        }
    }
}

impl Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let mut span = CapturedSpan {
            name: attrs.metadata().name(),
            fields: HashMap::new(),
        };
        attrs.record(&mut FieldVisitor(&mut span.fields));
        self.shared.spans.lock().unwrap().push(span);
        Id::from_u64(id)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        let index = span.into_u64() as usize - 1;
        let mut spans = self.shared.spans.lock().unwrap();
        values.record(&mut FieldVisitor(&mut spans[index].fields));
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

struct FieldVisitor<'a>(&'a mut HashMap<&'static str, String>);

impl Visit for FieldVisitor<'_> {
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name(), value.to_string());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name(), value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.insert(field.name(), format!("{value:?}"));
    }
}

pub(crate) fn capture<T>(f: impl FnOnce() -> T) -> (T, CapturedTrace) {
    let subscriber = CaptureSubscriber::new();
    let result = tracing::subscriber::with_default(subscriber.clone(), f);
    (result, subscriber.trace())
}
