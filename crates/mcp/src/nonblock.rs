use std::fs::File;
use std::io::Read;
use std::os::fd::AsFd;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, Interest, ReadBuf};

#[derive(Debug)]
pub(crate) struct StdinPipe {
    fd: tokio::io::unix::AsyncFd<File>,
}

pub(crate) fn reader() -> std::io::Result<StdinPipe> {
    let file = File::from(std::io::stdin().as_fd().try_clone_to_owned()?);
    Ok(StdinPipe {
        fd: nonblocking(file, Interest::READABLE)?,
    })
}

#[expect(deprecated)]
fn nonblocking(file: File, interest: Interest) -> std::io::Result<tokio::io::unix::AsyncFd<File>> {
    let bits = nix::fcntl::fcntl(&file, nix::fcntl::FcntlArg::F_GETFL)?;
    let flags = nix::fcntl::OFlag::from_bits_retain(bits) | nix::fcntl::OFlag::O_NONBLOCK;
    nix::fcntl::fcntl(&file, nix::fcntl::FcntlArg::F_SETFL(flags))?;
    tokio::io::unix::AsyncFd::with_interest(file, interest)
}

impl AsyncRead for StdinPipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let pipe = self.get_mut();
        loop {
            let mut guard = std::task::ready!(pipe.fd.poll_read_ready_mut(cx))?;
            match guard.try_io(|async_fd| Read::read(async_fd.get_mut(), buf.initialize_unfilled()))
            {
                Ok(Ok(count)) => {
                    buf.advance(count);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_blocked) => {}
            }
        }
    }
}
