use core::fmt;

use cealn_protocol::{
    event::{BuildEvent, BuildEventData, BuildEventSource},
    query::StdioLine,
};
use chrono::Utc;
use futures::channel::mpsc;

pub struct EventContext {
    dest: mpsc::UnboundedSender<BuildEvent>,
    current_source: Option<BuildEventSource>,
}

impl EventContext {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<BuildEvent>) {
        let (tx, rx) = mpsc::unbounded();
        let context = EventContext {
            dest: tx,
            current_source: None,
        };
        (context, rx)
    }

    pub fn send(&mut self, event: BuildEventData) {
        self.send_full(BuildEvent {
            timestamp: Utc::now(),
            source: self.current_source.clone(),
            data: event,
        })
    }

    pub fn send_full(&mut self, event: BuildEvent) {
        let _ = self.dest.unbounded_send(event);
    }

    pub fn send_stderr(&mut self, message: String) {
        self.send(BuildEventData::Stdio {
            line: StdioLine {
                stream: cealn_protocol::query::StdioStreamType::Stderr,
                contents: message.into_bytes(),
            },
        });
    }

    pub fn set_source(&mut self, source: BuildEventSource) {
        self.current_source = Some(source);
    }

    pub fn fork(&self) -> EventContext {
        EventContext {
            dest: self.dest.clone(),
            current_source: self.current_source.clone(),
        }
    }
}

impl fmt::Debug for EventContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO: identify destination so we can distinguish where events are routed in logs
        f.debug_struct("EventContext").finish()
    }
}
