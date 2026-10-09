//! Flow-controlled process streams for hosts reached through a private SSH TCP forward.
use bytes::Bytes;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(super) struct Reader {
    stream: h2::RecvStream,
    bytes: Bytes,
}

impl Reader {
    pub const fn new(stream: h2::RecvStream) -> Self {
        Self {
            stream,
            bytes: Bytes::new(),
        }
    }
}

impl AsyncRead for Reader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        while self.bytes.is_empty() {
            match std::task::ready!(self.stream.poll_data(cx)) {
                Some(Ok(bytes)) => self.bytes = bytes,
                Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
                None => return Poll::Ready(Ok(())),
            }
        }
        let count = self.bytes.len().min(output.remaining());
        self.stream
            .flow_control()
            .release_capacity(count)
            .map_err(io::Error::other)?;
        output.put_slice(&self.bytes.split_to(count));
        Poll::Ready(Ok(()))
    }
}

pub(super) struct Writer {
    stream: h2::SendStream<Bytes>,
    ended: bool,
}

impl Writer {
    pub const fn new(stream: h2::SendStream<Bytes>) -> Self {
        Self {
            stream,
            ended: false,
        }
    }
}

impl AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.ended {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        self.stream.reserve_capacity(bytes.len());
        while self.stream.capacity() == 0 {
            match std::task::ready!(self.stream.poll_capacity(cx)) {
                Some(Ok(_)) => {}
                Some(Err(error)) => return Poll::Ready(Err(io::Error::other(error))),
                None => return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
            }
        }
        let count = self.stream.capacity().min(bytes.len());
        let Some(bytes) = bytes.get(..count) else {
            return Poll::Ready(Err(io::ErrorKind::InvalidData.into()));
        };
        self.stream
            .send_data(Bytes::copy_from_slice(bytes), false)
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(count))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.ended {
            self.stream
                .send_data(Bytes::new(), true)
                .map_err(io::Error::other)?;
            self.ended = true;
        }
        Poll::Ready(Ok(()))
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if !self.ended {
            self.stream.send_reset(h2::Reason::CANCEL);
        }
    }
}
