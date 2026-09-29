//! A tracing subscriber for tests that records every event it sees.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// One recorded event: its level and its fields rendered as `name=value `.
#[derive(Debug, Clone)]
pub(crate) struct CapturedEvent {
    pub level: Level,
    pub fields: String,
}

/// Install with `tracing::subscriber::with_default` or `set_default`, then
/// read what was logged with [`EventCapture::events`].
#[derive(Clone, Default)]
pub(crate) struct EventCapture(Arc<Mutex<Vec<CapturedEvent>>>);

impl EventCapture {
    pub(crate) fn events(&self) -> Vec<CapturedEvent> {
        self.0.lock().expect("event capture lock").clone()
    }

    /// Every recorded event's fields, one event per line.
    pub(crate) fn text(&self) -> String {
        self.events()
            .iter()
            .map(|event| format!("{}\n", event.fields))
            .collect()
    }
}

struct FieldVisitor<'a>(&'a mut String);

impl Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

impl Subscriber for EventCapture {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = String::new();
        event.record(&mut FieldVisitor(&mut fields));
        self.0
            .lock()
            .expect("event capture lock")
            .push(CapturedEvent {
                level: *event.metadata().level(),
                fields,
            });
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}
