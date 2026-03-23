use nix::pty::openpty;
use nix::unistd::{dup2, execvp, fork, setsid, ForkResult};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;
use tokio::io::unix::AsyncFd;

/// Async-compatible PTY master handle. Cloneable for concurrent read/write.
#[derive(Clone)]
pub struct PtyMaster {
    fd: Arc<AsyncFd<OwnedFd>>,
}

impl PtyMaster {
    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|inner| {
                let fd = inner.get_ref().as_raw_fd();
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    pub async fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut offset = 0;
        while offset < data.len() {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|inner| {
                let fd = inner.get_ref().as_raw_fd();
                let n = unsafe {
                    libc::write(fd, data[offset..].as_ptr() as *const _, data.len() - offset)
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) => offset += n,
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.fd.get_ref().as_raw_fd(), libc::TIOCSWINSZ, &ws);
        }
    }
}

/// A child process connected to a PTY.
pub struct PtyProcess {
    master: PtyMaster,
    pid: nix::unistd::Pid,
}

impl PtyProcess {
    /// Spawn a command in a new PTY with the given initial size.
    pub fn spawn(program: &str, args: &[&str], cols: u16, rows: u16) -> io::Result<Self> {
        let pty = openpty(None, None).map_err(nix_to_io)?;
        let master_raw = pty.master.into_raw_fd();
        let slave_raw = pty.slave.into_raw_fd();

        // Set initial window size
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(master_raw, libc::TIOCSWINSZ, &ws);
        }

        // Prepare CStrings before fork to avoid allocating in the child.
        let c_program = CString::new(program).unwrap();
        let c_args: Vec<CString> = std::iter::once(program)
            .chain(args.iter().copied())
            .map(|s| CString::new(s).unwrap())
            .collect();

        match unsafe { fork() }.map_err(nix_to_io)? {
            ForkResult::Parent { child } => {
                // Close slave in parent
                unsafe { libc::close(slave_raw); }

                // Set master non-blocking
                let flags = unsafe { libc::fcntl(master_raw, libc::F_GETFL) };
                unsafe { libc::fcntl(master_raw, libc::F_SETFL, flags | libc::O_NONBLOCK) };

                let owned = unsafe { OwnedFd::from_raw_fd(master_raw) };
                let async_fd = AsyncFd::new(owned)?;

                Ok(PtyProcess {
                    master: PtyMaster {
                        fd: Arc::new(async_fd),
                    },
                    pid: child,
                })
            }
            ForkResult::Child => {
                unsafe { libc::close(master_raw); }
                let _ = setsid();
                unsafe { libc::ioctl(slave_raw, libc::TIOCSCTTY, 0); }
                let _ = dup2(slave_raw, 0);
                let _ = dup2(slave_raw, 1);
                let _ = dup2(slave_raw, 2);
                if slave_raw > 2 {
                    unsafe { libc::close(slave_raw); }
                }

                // Close inherited FDs to avoid leaking lock files etc.
                for fd in 3..1024 {
                    unsafe { libc::close(fd); }
                }

                // Ensure tmux sees a capable terminal
                unsafe {
                    libc::setenv(
                        c"TERM".as_ptr(),
                        c"xterm-256color".as_ptr(),
                        1,
                    );
                }

                let _ = execvp(&c_program, &c_args);
                unsafe { libc::_exit(1); }
            }
        }
    }

    pub fn master(&self) -> PtyMaster {
        self.master.clone()
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        let _ = nix::sys::signal::kill(self.pid, nix::sys::signal::Signal::SIGTERM);
        // Brief blocking wait to reap the child and avoid zombies.
        // Try non-blocking first; if still alive, sleep briefly and retry.
        for _ in 0..10 {
            match nix::sys::wait::waitpid(
                self.pid,
                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
            ) {
                Ok(nix::sys::wait::WaitStatus::StillAlive) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                _ => return,
            }
        }
        // Final attempt — if still alive after 500ms, SIGKILL.
        let _ = nix::sys::signal::kill(self.pid, nix::sys::signal::Signal::SIGKILL);
        let _ = nix::sys::wait::waitpid(self.pid, None);
    }
}

fn nix_to_io(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}
