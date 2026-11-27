//! Tests: a subscriber that takes every event and formats its fields. The
//! logging macros evaluate their arguments only when someone listens, so
//! without one a line that indexes past an array's length passes the tests
//! and panics only on the card, where the log is on (run integ-p15a-def).

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Runs `f` with every line of this thread made and formatted.
pub(crate) fn with_every_line(f: impl FnOnce()) {
    tracing::subscriber::with_default(Every::default(), f);
}

#[derive(Default)]
struct Every(AtomicU64);

impl tracing::Subscriber for Every {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1 + self.0.fetch_add(1, Relaxed))
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut Formatted::default());
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// An event's fields, formatted (and dropped).
#[derive(Default)]
struct Formatted(String);

impl tracing::field::Visit for Formatted {
    fn record_debug(&mut self, _: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let _ = write!(self.0, "{value:?}");
    }
}

#[cfg(test)]
mod tests {
    /// The lines' arguments are evaluated under it, and not without it.
    #[test]
    fn a_line_is_made() {
        let made = std::cell::Cell::new(0);
        let count = || {
            made.set(made.get() + 1);
            0
        };
        super::with_every_line(|| tracing::info!("a line {}", count()));
        assert_eq!(made.get(), 1);
    }
}
