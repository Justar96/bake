//! Inherited pipe handles are synchronous. Keep their writes on one owned
//! thread and cancel that thread's I/O until it exits, closing the race where
//! cancellation arrives just before WriteFile starts. No pool thread is used.

use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
use windows_sys::Win32::System::Console::{GetConsoleMode, WriteConsoleW};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

pub struct Writer {
    streams: [File; 2],
    consoles: [bool; 2],
}

pub struct Wake(OwnedHandle);

impl Writer {
    pub fn new() -> io::Result<(Self, ())> {
        let streams = [
            File::from(io::stdout().as_handle().try_clone_to_owned()?),
            File::from(io::stderr().as_handle().try_clone_to_owned()?),
        ];
        let consoles = std::array::from_fn(|index| {
            let mut mode = 0;
            // SAFETY: the file handle and mode allocation are live.
            unsafe { GetConsoleMode(streams[index].as_raw_handle(), &mut mode) != 0 }
        });
        Ok((Self { streams, consoles }, ()))
    }

    pub fn write(&mut self, stream: usize, mut bytes: &[u8], stop: &AtomicBool) -> io::Result<()> {
        if self.consoles[stream] {
            let text = std::str::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            let text: Vec<u16> = text.encode_utf16().collect();
            let mut remaining = text.as_slice();
            while !remaining.is_empty() {
                if stop.load(Ordering::Acquire) {
                    return Err(super::stopped());
                }
                let mut written = 0;
                // SAFETY: the handle, UTF-16 slice and count remain live until
                // this synchronous call returns, including cancellation.
                if unsafe {
                    WriteConsoleW(
                        self.streams[stream].as_raw_handle(),
                        remaining.as_ptr().cast(),
                        remaining.len() as u32,
                        &mut written,
                        std::ptr::null(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if written == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }
                remaining = &remaining[written as usize..];
            }
        } else {
            while !bytes.is_empty() {
                if stop.load(Ordering::Acquire) {
                    return Err(super::stopped());
                }
                match self.streams[stream].write(bytes) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(written) => bytes = &bytes[written..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }
}

impl Wake {
    pub fn attach((): (), thread: &JoinHandle<()>) -> io::Result<Self> {
        // Duplicate the thread handle so the signal owner has its own lifetime.
        Ok(Self(thread.as_handle().try_clone_to_owned()?))
    }

    pub fn cancel(&self) {
        loop {
            // SAFETY: this owned handle names only the print writer thread.
            let state = unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) };
            if state != WAIT_TIMEOUT {
                break;
            }
            // It may be between calls when cancellation first arrives. The
            // worker checks the stop flag before every call; retry cancellation
            // until it has exited so that final check/write race is covered.
            unsafe { CancelSynchronousIo(self.0.as_raw_handle()) };
            unsafe { WaitForSingleObject(self.0.as_raw_handle(), 10) };
        }
    }
}
