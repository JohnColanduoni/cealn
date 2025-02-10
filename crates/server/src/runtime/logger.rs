//! Implements logging parsing for stdout/stderr output of runtimes
//!
//!
//! # Parsing
//!
//! Performs the following parsing:
//!
//! Lines are split on `\r\n`, `\n`, or `\r` (to handle full line edits)
//! The terminators are removed from the output.

use std::{
    io, mem,
    sync::{Arc, Mutex},
};

use cealn_event::{BuildEventData, EventContext};
use cealn_protocol::query::{StdioLine, StdioStreamType};
use futures::{channel::mpsc, executor, prelude::*, Stream};
use tracing::{trace, warn};

use cealn_runtime::api::Handle;
use cealn_runtime_virt::fs::print::PrintHandle;

const LOGGER_ENTRY_BUFFER: usize = 4096;

/// Implements stdout/stderr processing from runtimes
pub struct Logger {
    shared: Arc<Mutex<Shared>>,
}

struct Shared {
    lines: Vec<StdioLine>,
    events: EventContext,
}

struct WriteHandle {
    data: Vec<u8>,
    previous_line_trailing_cr: bool,
    stream: StdioStreamType,
    shared: Arc<Mutex<Shared>>,
}

impl Logger {
    pub fn new(events: EventContext) -> (Logger, Arc<dyn Handle>, Arc<dyn Handle>) {
        let shared = Arc::new(Mutex::new(Shared {
            lines: Vec::new(),
            events,
        }));
        let stdout_handle = WriteHandle {
            data: Vec::new(),
            previous_line_trailing_cr: false,
            stream: StdioStreamType::Stdout,
            shared: shared.clone(),
        };
        let stderr_handle = WriteHandle {
            data: Vec::new(),
            previous_line_trailing_cr: false,
            stream: StdioStreamType::Stdout,
            shared: shared.clone(),
        };

        let logger = Logger { shared };
        let stdout_handle = PrintHandle::new(stdout_handle).to_handle();
        let stderr_handle = PrintHandle::new(stderr_handle).to_handle();
        (logger, stdout_handle, stderr_handle)
    }

    pub fn finish(self) -> Vec<StdioLine> {
        let inner = Arc::try_unwrap(self.shared)
            .unwrap_or_else(|_| panic!("handles to stdio still exist"))
            .into_inner()
            .unwrap();
        inner.lines
    }
}

impl WriteHandle {
    #[tracing::instrument(level = "trace", skip(self))]
    fn push_entry(&mut self, contents: Vec<u8>) {
        let mut shared = self.shared.lock().unwrap();
        let line = StdioLine {
            stream: self.stream,
            contents,
        };
        shared.events.send(BuildEventData::Stdio { line: line.clone() });
        shared.lines.push(line);
    }
}

impl io::Write for WriteHandle {
    #[tracing::instrument(level = "trace", err, skip(self))]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut rem_buf = buf;

        if self.previous_line_trailing_cr {
            match rem_buf.split_first() {
                Some((b'\n', tail)) => {
                    // This newline is part of a \r\n sequence stradling two writes. Ignore it
                    rem_buf = tail;
                    self.previous_line_trailing_cr = false;
                }
                Some(_) => {
                    self.previous_line_trailing_cr = false;
                }
                None => {
                    // Empty write, don't clear flag and early-out
                    return Ok(0);
                }
            }
        }

        // TODO: perhaps combine this scan with utf-8 validation to avoid multiple scans over the data
        while let Some((newline_index, &terminator)) =
            rem_buf.iter().enumerate().find(|(_, x)| **x == b'\n' || **x == b'\r')
        {
            let (head, tail) = rem_buf.split_at(newline_index);
            self.data.extend_from_slice(head);
            if terminator == b'\r' {
                match tail.get(1) {
                    Some(b'\n') => {
                        rem_buf = &tail[2..];
                    }
                    Some(_) => {
                        rem_buf = &tail[1..];
                    }
                    None => {
                        // In this case, we cannot be sure whether we are encountering a \r or a \r\n. Set a flag so
                        // future writes can ignore the leading \n if necessary.
                        self.previous_line_trailing_cr = true;
                    }
                }
            } else {
                rem_buf = &tail[1..];
            }

            let line = mem::replace(&mut self.data, Vec::new());
            self.push_entry(line);
        }

        // TODO: implement maximum line buffer size
        self.data.extend_from_slice(rem_buf);

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
