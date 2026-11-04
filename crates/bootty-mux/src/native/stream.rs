//! Bounded blocking adapter used only by mux workers, for any asynchronous duplex.
use super::backend::Duplex;
use anyhow::Result;
use std::{
    io::{self, Read, Write},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub struct AsyncConnection<T> {
    runtime: Arc<tokio::runtime::Runtime>,
    stream: T,
    timeout: Duration,
}
impl<T> AsyncConnection<T> {
    #[must_use]
    pub const fn new(runtime: Arc<tokio::runtime::Runtime>, stream: T) -> Self {
        Self {
            runtime,
            stream,
            timeout: Duration::from_secs(10),
        }
    }
}
impl<T: AsyncRead + Unpin> Read for AsyncConnection<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.runtime.block_on(async {
            tokio::time::timeout(self.timeout, self.stream.read(bytes))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "mux stream idle"))?
        })
    }
}
impl<T: AsyncWrite + Unpin> Write for AsyncConnection<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), self.stream.write(bytes))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "mux write timed out"))?
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        self.runtime.block_on(self.stream.flush())
    }
}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Duplex for AsyncConnection<T> {
    fn split(self: Box<Self>) -> Result<super::backend::SplitDuplex> {
        let (read, write) = tokio::io::split(self.stream);
        let (stop, stopped) = tokio::sync::watch::channel(false);
        Ok((
            Box::new(BlockingHalf {
                runtime: Arc::clone(&self.runtime),
                stream: read,
                stopped: stopped.clone(),
            }),
            Box::new(BlockingHalf {
                runtime: self.runtime,
                stream: write,
                stopped,
            }),
            Arc::new(move || {
                let _ = stop.send(true);
            }),
        ))
    }
    fn read_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.timeout = timeout;
        Ok(())
    }
}

struct BlockingHalf<T> {
    runtime: Arc<tokio::runtime::Runtime>,
    stream: T,
    stopped: tokio::sync::watch::Receiver<bool>,
}
impl<T: AsyncRead + Unpin> Read for BlockingHalf<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = self.stopped.wait_for(|stopped| *stopped) => Ok(0),
                result = self.stream.read(bytes) => result,
            }
        })
    }
}
impl<T: AsyncWrite + Unpin> Write for BlockingHalf<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = self.stopped.wait_for(|stopped| *stopped) => Err(io::Error::new(io::ErrorKind::BrokenPipe, "mux stream closed")),
                result = self.stream.write(bytes) => result,
            }
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        self.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = self.stopped.wait_for(|stopped| *stopped) => Err(io::Error::new(io::ErrorKind::BrokenPipe, "mux stream closed")),
                result = self.stream.flush() => result,
            }
        })
    }
}
