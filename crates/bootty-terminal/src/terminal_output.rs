//! Ordered terminal transport events. Control boundaries stay in the same queue
//! as bytes so a reconnect or resize cannot overtake published output.
use crate::pty_backlog::{DrainStats, OutputBacklog, drain_output_backlog_with_limits};
use std::{
    collections::VecDeque,
    io::{self, Read},
    time::Instant,
};

pub enum TerminalOutput {
    Bytes { bytes: Vec<u8> },
}
impl TerminalOutput {
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::Bytes { bytes, .. } => bytes.len(),
        }
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub enum TerminalReader {
    Bytes(Box<dyn Read + Send>),
}
impl TerminalReader {
    /// # Errors
    /// Returns the underlying read or transport failure.
    pub fn next_output(&mut self) -> io::Result<Option<TerminalOutput>> {
        match self {
            Self::Bytes(reader) => {
                let mut bytes = vec![0; 8192];
                let count = loop {
                    match reader.read(&mut bytes) {
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        result => break result?,
                    }
                };
                if count == 0 {
                    return Ok(None);
                }
                bytes.truncate(count);
                Ok(Some(TerminalOutput::Bytes { bytes }))
            }
        }
    }
}

pub(crate) enum OutputSlice<'a> {
    Bytes(&'a [u8]),
}

pub(crate) struct OutputQueue {
    data: OutputBacklog,
    events: VecDeque<TerminalOutput>,
    queued_bytes: usize,
}
impl OutputQueue {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            data: OutputBacklog::with_capacity(capacity),
            events: VecDeque::with_capacity(capacity),
            queued_bytes: 0,
        }
    }
    pub const fn len(&self) -> usize {
        self.data.len().saturating_add(self.queued_bytes)
    }
    pub fn is_empty(&self) -> bool {
        self.data.is_empty() && self.events.is_empty()
    }
    pub fn push_back(&mut self, event: TerminalOutput) {
        self.queued_bytes = self.queued_bytes.saturating_add(event.len());
        self.events.push_back(event);
    }
    pub fn clear(&mut self) {
        self.data.clear();
        self.events.clear();
        self.queued_bytes = 0;
    }
    fn pop(&mut self) -> Option<TerminalOutput> {
        let event = self.events.pop_front()?;
        self.queued_bytes = self.queued_bytes.saturating_sub(event.len());
        Some(event)
    }
    pub fn drain(
        &mut self,
        max_bytes: usize,
        max_chunks: usize,
        max_time_us: u128,
        mut apply: impl FnMut(OutputSlice<'_>),
    ) -> DrainStats {
        let started = Instant::now();
        let mut stats = DrainStats::default();
        while !self.is_empty()
            && stats.bytes < max_bytes
            && stats.chunks < max_chunks
            && started.elapsed().as_micros() < max_time_us
        {
            if self.data.is_empty() {
                match self.pop() {
                    Some(TerminalOutput::Bytes { bytes }) => {
                        self.data.push_back(bytes);
                        while let Some(TerminalOutput::Bytes { bytes }) = self.pop() {
                            self.data.push_back(bytes);
                        }
                    }
                    None => break,
                }
            }
            let drained = drain_output_backlog_with_limits(
                &mut self.data,
                max_bytes.saturating_sub(stats.bytes),
                max_chunks.saturating_sub(stats.chunks),
                max_time_us.saturating_sub(started.elapsed().as_micros()),
                |bytes| apply(OutputSlice::Bytes(bytes)),
            );
            stats.bytes = stats.bytes.saturating_add(drained.bytes);
            stats.chunks = stats.chunks.saturating_add(drained.chunks);
            if drained.bytes == 0 {
                break;
            }
        }
        stats.elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        stats
    }
}
