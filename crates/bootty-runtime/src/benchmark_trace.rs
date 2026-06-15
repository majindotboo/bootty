use std::{
    env,
    fs::File,
    io::{self, BufWriter, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

pub const BOOTTY_BENCH_TRACE_ENV: &str = "BOOTTY_BENCH_TRACE";
pub const BOOTTY_BENCH_TRACE_SAMPLE_EVERY_ENV: &str = "BOOTTY_BENCH_TRACE_SAMPLE_EVERY";

#[derive(Clone, Debug)]
pub struct BenchmarkTrace {
    inner: Arc<BenchmarkTraceInner>,
}

#[derive(Debug)]
struct BenchmarkTraceInner {
    writer: Mutex<BufWriter<File>>,
    start: Instant,
    sample_every: usize,
    events: AtomicUsize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceValue<'a> {
    Bool(bool),
    U64(u64),
    Usize(usize),
    Str(&'a str),
}

impl BenchmarkTrace {
    pub fn from_env() -> io::Result<Option<Self>> {
        let Ok(path) = env::var(BOOTTY_BENCH_TRACE_ENV) else {
            return Ok(None);
        };
        let path = path.trim();
        if path.is_empty() {
            return Ok(None);
        }
        let sample_every = env::var(BOOTTY_BENCH_TRACE_SAMPLE_EVERY_ENV)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(1);
        Self::create(path, sample_every).map(Some)
    }

    pub fn create(path: impl AsRef<Path>, sample_every: usize) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            inner: Arc::new(BenchmarkTraceInner {
                writer: Mutex::new(BufWriter::new(file)),
                start: Instant::now(),
                sample_every: sample_every.max(1),
                events: AtomicUsize::new(0),
            }),
        })
    }

    pub fn emit(&self, event: &str, fields: &[(&str, TraceValue<'_>)]) {
        let index = self.inner.events.fetch_add(1, Ordering::Relaxed);
        if !index.is_multiple_of(self.inner.sample_every) {
            return;
        }
        let Ok(mut writer) = self.inner.writer.lock() else {
            return;
        };

        let elapsed_ns = self.inner.start.elapsed().as_nanos();
        let _ = write!(
            writer,
            "{{\"schema_version\":1,\"ts_ns\":{elapsed_ns},\"event\":\""
        );
        let _ = write_json_string(&mut *writer, event);
        let _ = write!(writer, "\"");
        for (key, value) in fields {
            let _ = write!(writer, ",\"");
            let _ = write_json_string(&mut *writer, key);
            let _ = write!(writer, "\":");
            let _ = write_trace_value(&mut *writer, *value);
        }
        let _ = writeln!(writer, "}}");
        let _ = writer.flush();
    }
}

fn write_trace_value(writer: &mut impl Write, value: TraceValue<'_>) -> io::Result<()> {
    match value {
        TraceValue::Bool(value) => write!(writer, "{value}"),
        TraceValue::U64(value) => write!(writer, "{value}"),
        TraceValue::Usize(value) => write!(writer, "{value}"),
        TraceValue::Str(value) => {
            write!(writer, "\"")?;
            write_json_string(writer, value)?;
            write!(writer, "\"")
        }
    }
}

fn write_json_string(writer: &mut impl Write, value: &str) -> io::Result<()> {
    for ch in value.chars() {
        match ch {
            '"' => writer.write_all(br#"\""#)?,
            '\\' => writer.write_all(br#"\\"#)?,
            '\n' => writer.write_all(br#"\n"#)?,
            '\r' => writer.write_all(br#"\r"#)?,
            '\t' => writer.write_all(br#"\t"#)?,
            ch if ch.is_control() => write!(writer, "\\u{:04x}", ch as u32)?,
            ch => write!(writer, "{ch}")?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_trace_writes_jsonl_records() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("trace.jsonl");
        let trace = BenchmarkTrace::create(&path, 1)?;

        trace.emit(
            "pty_read",
            &[
                ("bytes", TraceValue::Usize(8192)),
                ("escaped", TraceValue::Str("quote \" slash \\ newline\n")),
                ("active", TraceValue::Bool(true)),
            ],
        );

        let contents = std::fs::read_to_string(path)?;
        assert!(contents.contains("\"schema_version\":1"));
        assert!(contents.contains("\"event\":\"pty_read\""));
        assert!(contents.contains("\"bytes\":8192"));
        assert!(contents.contains(r#"quote \" slash \\ newline\n"#));
        assert!(contents.contains("\"active\":true"));
        Ok(())
    }

    #[test]
    fn benchmark_trace_sampling_skips_unsampled_events() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("trace.jsonl");
        let trace = BenchmarkTrace::create(&path, 2)?;

        trace.emit("first", &[]);
        trace.emit("second", &[]);
        trace.emit("third", &[]);

        let contents = std::fs::read_to_string(path)?;
        assert_eq!(contents.lines().count(), 2);
        assert!(contents.contains("\"event\":\"first\""));
        assert!(!contents.contains("\"event\":\"second\""));
        assert!(contents.contains("\"event\":\"third\""));
        Ok(())
    }
}
