//! Nonblocking writes wait on both pipe capacity and an owned cancellation
//! socket. Duplicated stdout/stderr share their open-file descriptions with
//! the caller: change flags only where needed and restore them on every exit.

use std::fs::File;
use std::io::{self, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

struct Stream {
    file: File,
    restore_flags: Option<libc::c_int>,
    needs_nonblocking: bool,
}

impl Stream {
    fn new(fd: BorrowedFd<'_>) -> io::Result<Self> {
        let file = File::from(fd.try_clone_to_owned()?);
        let raw = file.as_raw_fd();
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the descriptor and stat allocation are live for the call.
        if unsafe { libc::fstat(raw, stat.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstat initialized the allocation on success.
        let mode = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
        // Regular files and devices such as /dev/null need no flag change.
        let can_wait =
            matches!(mode, libc::S_IFIFO | libc::S_IFSOCK) || unsafe { libc::isatty(raw) } == 1;
        Ok(Self {
            file,
            restore_flags: None,
            needs_nonblocking: can_wait,
        })
    }

    fn prepare(&mut self) -> io::Result<()> {
        if self.needs_nonblocking {
            // Defer flag changes until the first write, after signal handlers
            // have registered. A failed startup leaves inherited flags alone.
            let raw = self.file.as_raw_fd();
            // SAFETY: fcntl operates on this owned, live descriptor.
            let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
            if flags == -1 {
                return Err(io::Error::last_os_error());
            }
            if flags & libc::O_NONBLOCK == 0 {
                if unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
                    return Err(io::Error::last_os_error());
                }
                self.restore_flags = Some(flags);
            }
            self.needs_nonblocking = false;
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(flags) = self.restore_flags {
            // SAFETY: the file still owns this descriptor during Drop.
            let _ = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_SETFL, flags) };
        }
    }
}

pub struct Writer {
    streams: [Stream; 2],
    stop: UnixStream,
}

pub struct Wake(UnixStream);

impl Writer {
    pub fn new() -> io::Result<(Self, UnixStream)> {
        let streams = [
            Stream::new(io::stdout().as_fd())?,
            Stream::new(io::stderr().as_fd())?,
        ];
        let (stop, wake) = UnixStream::pair()?;
        Ok((Self { streams, stop }, wake))
    }

    pub fn write(&mut self, stream: usize, mut bytes: &[u8], stop: &AtomicBool) -> io::Result<()> {
        let stream = &mut self.streams[stream];
        stream.prepare()?;
        let file = &mut stream.file;
        while !bytes.is_empty() {
            if stop.load(Ordering::Acquire) {
                return Err(super::stopped());
            }
            match file.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut descriptors = [
                        libc::pollfd {
                            fd: file.as_raw_fd(),
                            events: libc::POLLOUT,
                            revents: 0,
                        },
                        libc::pollfd {
                            fd: self.stop.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        },
                    ];
                    // SAFETY: both descriptors and the array remain live during poll.
                    if unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) } == -1 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

impl Wake {
    pub fn attach(wake: UnixStream, _thread: &JoinHandle<()>) -> io::Result<Self> {
        Ok(Self(wake))
    }

    pub fn cancel(&self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}
