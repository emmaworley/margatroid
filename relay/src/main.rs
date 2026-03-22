//! margatroid-relay: PTY interceptor that owns the session's PTY master.
//!
//! Usage: margatroid-relay <session-name> <command> [args...]
//!
//! The relay:
//! 1. Creates a PTY pair and forks the command on the slave side
//! 2. Relays stdin/stdout ↔ PTY master (so tmux sees a normal terminal)
//! 3. Listens on a Unix socket for web clients (fan-out output, merge input)
//! 4. Maintains a ring buffer for scrollback replay on client connect

use nix::pty::openpty;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{dup2, execvp, fork, setsid, ForkResult, Pid};
use std::collections::VecDeque;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex};

const RING_BUFFER_SIZE: usize = 64 * 1024;
const MAX_CLIENTS: usize = 8;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: margatroid-relay <session-name> <command> [args...]");
        std::process::exit(1);
    }

    let session_name = &args[1];
    let command = &args[2];
    let command_args = &args[2..];

    // Compute socket path.
    let margatroid_dir = std::env::var("MARGATROID_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join(".margatroid")
        });
    let sock_path = margatroid_dir
        .join("sessions")
        .join(session_name)
        .join("relay.sock");

    // Remove stale socket.
    let _ = std::fs::remove_file(&sock_path);

    // Create PTY pair.
    let pty = openpty(None, None).expect("openpty failed");
    let master_raw = pty.master.into_raw_fd();
    let slave_raw = pty.slave.into_raw_fd();

    // Inherit terminal size from stdin (tmux's PTY).
    inherit_winsize(libc::STDIN_FILENO, master_raw);

    // Fork the child process.
    let child_pid = match unsafe { fork() }.expect("fork failed") {
        ForkResult::Child => {
            unsafe { libc::close(master_raw) };
            let _ = setsid();
            unsafe { libc::ioctl(slave_raw, libc::TIOCSCTTY, 0) };
            let _ = dup2(slave_raw, 0);
            let _ = dup2(slave_raw, 1);
            let _ = dup2(slave_raw, 2);
            if slave_raw > 2 {
                unsafe { libc::close(slave_raw) };
            }
            // Close inherited FDs 3..1024 to avoid leaking locks.
            for fd in 3..1024 {
                unsafe { libc::close(fd) };
            }
            std::env::set_var("TERM", "xterm-256color");

            let c_cmd = CString::new(command.as_str()).unwrap();
            let c_args: Vec<CString> = command_args
                .iter()
                .map(|s| CString::new(s.as_str()).unwrap())
                .collect();
            let _ = execvp(&c_cmd, &c_args);
            unsafe { libc::_exit(127) };
        }
        ForkResult::Parent { child } => {
            unsafe { libc::close(slave_raw) };
            child
        }
    };

    // Set master to non-blocking for async I/O.
    set_nonblocking(master_raw);

    // Put our stdin into raw mode so bytes pass through transparently.
    // Without this, the terminal line discipline buffers input, echoes
    // characters, and intercepts signals — breaking Claude Code's TUI.
    let orig_termios = set_stdin_raw();

    // Run the async event loop.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime failed");
    let exit_code = rt.block_on(event_loop(master_raw, child_pid, sock_path));

    // Restore terminal before exit.
    if let Some(t) = orig_termios {
        let _ = nix::sys::termios::tcsetattr(
            std::io::stdin(),
            nix::sys::termios::SetArg::TCSANOW,
            &t,
        );
    }

    std::process::exit(exit_code);
}

/// Put stdin into raw mode, returning the original termios for later restore.
fn set_stdin_raw() -> Option<nix::sys::termios::Termios> {
    use nix::sys::termios::{self, SetArg};
    use std::io;

    let fd = io::stdin();
    let orig = termios::tcgetattr(&fd).ok()?;
    let mut raw = orig.clone();
    termios::cfmakeraw(&mut raw);
    termios::tcsetattr(&fd, SetArg::TCSANOW, &raw).ok()?;
    Some(orig)
}

fn inherit_winsize(from_fd: RawFd, to_fd: RawFd) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(from_fd, libc::TIOCGWINSZ, &mut ws) } == 0 {
        unsafe { libc::ioctl(to_fd, libc::TIOCSWINSZ, &ws) };
    }
}

fn set_nonblocking(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
}

fn set_winsize(fd: RawFd, cols: u16, rows: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
}

/// Async wrapper for reading from a raw FD via tokio's AsyncFd.
struct AsyncRawFd {
    inner: tokio::io::unix::AsyncFd<OwnedFd>,
}

impl AsyncRawFd {
    fn new(fd: RawFd) -> std::io::Result<Self> {
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self {
            inner: tokio::io::unix::AsyncFd::new(owned)?,
        })
    }

    fn raw_fd(&self) -> RawFd {
        self.inner.get_ref().as_raw_fd()
    }

    async fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let mut guard = self.inner.readable().await?;
            match guard.try_io(|inner| {
                let fd = inner.get_ref().as_raw_fd();
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    async fn write_all(&self, data: &[u8]) -> std::io::Result<()> {
        let mut offset = 0;
        while offset < data.len() {
            let mut guard = self.inner.writable().await?;
            match guard.try_io(|inner| {
                let fd = inner.get_ref().as_raw_fd();
                let n = unsafe {
                    libc::write(fd, data[offset..].as_ptr() as *const _, data.len() - offset)
                };
                if n < 0 {
                    Err(std::io::Error::last_os_error())
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
}

type RingBuffer = Arc<Mutex<VecDeque<u8>>>;

async fn event_loop(master_fd: RawFd, child_pid: Pid, sock_path: PathBuf) -> i32 {
    let master = Arc::new(AsyncRawFd::new(master_fd).expect("AsyncFd failed"));
    let ring: RingBuffer = Arc::new(Mutex::new(VecDeque::with_capacity(RING_BUFFER_SIZE)));
    let (tx, _rx) = broadcast::channel::<Vec<u8>>(256);
    let client_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Unix socket listener for web clients.
    let listener = UnixListener::bind(&sock_path).expect("failed to bind relay socket");
    tracing::info!("relay socket at {}", sock_path.display());

    // Track the master fd for resize operations from clients.
    let master_raw_fd = master.raw_fd();

    // --- Task: read PTY master → write stdout + broadcast to clients ---
    let pty_reader_master = master.clone();
    let pty_reader_tx = tx.clone();
    let pty_reader_ring = ring.clone();
    let pty_reader = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader_master.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    // Write to stdout (tmux).
                    let _ = stdout.write_all(&chunk).await;
                    let _ = stdout.flush().await;
                    // Append to ring buffer.
                    {
                        let mut ring = pty_reader_ring.lock().await;
                        for &b in &chunk {
                            if ring.len() >= RING_BUFFER_SIZE {
                                ring.pop_front();
                            }
                            ring.push_back(b);
                        }
                    }
                    // Broadcast to web clients.
                    let _ = pty_reader_tx.send(chunk);
                }
                Err(e) => {
                    if e.raw_os_error() == Some(libc::EIO) {
                        break; // Child exited, PTY closed.
                    }
                    tracing::error!("pty read error: {e}");
                    break;
                }
            }
        }
    });

    // --- Task: read stdin (tmux) → write PTY master ---
    let stdin_writer_master = master.clone();
    let stdin_writer = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if stdin_writer_master.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // --- Task: accept Unix socket clients ---
    let socket_task = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };

            let count = client_count.load(std::sync::atomic::Ordering::Relaxed);
            if count >= MAX_CLIENTS {
                tracing::warn!("max clients reached, dropping connection");
                drop(stream);
                continue;
            }
            client_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            let client_master = master.clone();
            let client_ring = ring.clone();
            let client_rx = tx.subscribe();
            let client_count = client_count.clone();

            tokio::spawn(async move {
                handle_client(stream, client_master, client_ring, client_rx, master_raw_fd).await;
                client_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            });
        }
    });

    // --- Task: SIGWINCH from tmux → resize PTY ---
    let sigwinch_task = tokio::spawn(async move {
        let mut sig =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
                .expect("SIGWINCH handler failed");
        while sig.recv().await.is_some() {
            // tmux resized its pane; propagate to the inner PTY.
            inherit_winsize(libc::STDIN_FILENO, master_raw_fd);
        }
    });

    // --- Task: wait for child exit ---
    let child_task = tokio::task::spawn_blocking(move || {
        match waitpid(child_pid, None) {
            Ok(WaitStatus::Exited(_, code)) => code,
            Ok(WaitStatus::Signaled(_, sig, _)) => 128 + sig as i32,
            _ => 1,
        }
    });

    // Wait for child to exit, then clean up.
    let exit_code = child_task.await.unwrap_or(1);
    tracing::info!("child exited with code {exit_code}");

    // Clean up.
    pty_reader.abort();
    stdin_writer.abort();
    socket_task.abort();
    sigwinch_task.abort();
    let _ = std::fs::remove_file(&sock_path);

    exit_code
}

/// Handle a single web client connected via Unix socket.
///
/// Protocol:
/// - Raw bytes from client → written to PTY master (terminal input)
/// - Raw bytes from PTY → sent to client (terminal output, via broadcast)
/// - Control: \x00 + 2-byte LE cols + 2-byte LE rows → resize PTY
async fn handle_client(
    stream: UnixStream,
    master: Arc<AsyncRawFd>,
    ring: RingBuffer,
    mut rx: broadcast::Receiver<Vec<u8>>,
    master_raw_fd: RawFd,
) {
    let (mut reader, mut writer) = stream.into_split();

    // Replay scrollback buffer.
    {
        let ring = ring.lock().await;
        let (a, b) = ring.as_slices();
        if !a.is_empty() {
            let _ = writer.write_all(a).await;
        }
        if !b.is_empty() {
            let _ = writer.write_all(b).await;
        }
    }

    // Broadcast → client (output).
    let write_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(data) => {
                    if writer.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("client lagged {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Client → PTY master (input + resize control).
    let read_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let data = &buf[..n];
                    // Check for resize control message: \x00 + 4 bytes.
                    if data.len() == 5 && data[0] == 0x00 {
                        let cols = u16::from_le_bytes([data[1], data[2]]);
                        let rows = u16::from_le_bytes([data[3], data[4]]);
                        set_winsize(master_raw_fd, cols, rows);
                    } else {
                        let _ = master.write_all(data).await;
                    }
                }
                Err(_) => break,
            }
        }
    });

    tokio::select! {
        _ = write_task => {}
        _ = read_task => {}
    }
}
