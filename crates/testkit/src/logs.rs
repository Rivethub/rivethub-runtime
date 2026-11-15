use std::sync::Arc;

use parking_lot::Mutex;
use tracing_subscriber::layer::SubscriberExt as _;

pub type Lines = Arc<Mutex<Vec<String>>>;

struct Recorder(Lines);

struct Message(Lines, tracing::Level);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.lock().push(format!("{} {value:?}", self.1));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.lock().push(format!("{} {value}", self.1));
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for Recorder
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        event.record(&mut Message(Arc::clone(&self.0), *event.metadata().level()));
    }
}

pub fn recorded() -> (tracing::subscriber::DefaultGuard, Lines) {
    let lines = Lines::default();
    let subscriber = tracing_subscriber::registry().with(Recorder(Arc::clone(&lines)));
    (tracing::subscriber::set_default(subscriber), lines)
}
