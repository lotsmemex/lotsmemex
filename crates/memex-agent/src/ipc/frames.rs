//! Bounded owner-socket admission. Partial uploads never occupy the dispatcher.
use super::*;
use std::sync::{mpsc, Arc, atomic::{AtomicUsize, Ordering}};
use std::time::Instant;

struct Admission(Arc<AtomicUsize>);
impl Drop for Admission {
    fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); }
}

pub(super) struct Prepared {
    pub stream: UnixStream,
    pub command: Result<Command>,
    _admission: Admission,
}

pub(super) struct Incoming {
    count: Arc<AtomicUsize>,
    send: mpsc::SyncSender<Prepared>,
    receive: mpsc::Receiver<Prepared>,
}

impl Incoming {
    pub fn new() -> Self {
        let (send, receive) = mpsc::sync_channel(8);
        Self { count: Arc::new(AtomicUsize::new(0)), send, receive }
    }
    pub fn admit(&self, mut stream: UnixStream) -> Result<()> {
        if peer_uid(&stream)? != unsafe { libc::geteuid() } { bail!("peer_not_owner") }
        if self.count.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < 8).then_some(n + 1)).is_err() {
            // No command admitted; close so a write client preserves uncertainty.
            return Ok(());
        }
        let admission = Admission(self.count.clone());
        let send = self.send.clone();
        std::thread::spawn(move || {
            let command = read_command(&mut stream);
            let _ = send.send(Prepared { stream, command, _admission: admission });
        });
        Ok(())
    }
    pub fn next(&self) -> Option<Prepared> { self.receive.try_recv().ok() }
}

pub(super) fn read_command(stream: &mut UnixStream) -> Result<Command> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    if peer_uid(stream)? != unsafe { libc::geteuid() } { bail!("peer_not_owner") }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut read_exact = |mut out: &mut [u8]| -> Result<()> {
        while !out.is_empty() {
            let remaining = deadline.checked_duration_since(Instant::now()).ok_or_else(|| anyhow::anyhow!("request_timeout"))?;
            stream.set_read_timeout(Some(remaining))?;
            let n = stream.read(out)?;
            if n == 0 { bail!("truncated_frame") }
            out = &mut out[n..];
        }
        Ok(())
    };
    let mut header = [0; 4];
    read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_MARKDOWN_EDIT_FRAME { bail!("frame_too_large") }
    // Allocate only as bytes arrive, not the entire peer-advertised length.
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    let mut chunk = zeroize::Zeroizing::new([0u8; 64 * 1024]);
    while bytes.len() < len {
        let n = (len - bytes.len()).min(chunk.len());
        read_exact(&mut chunk[..n])?;
        bytes.extend_from_slice(&chunk[..n]);
    }
    let command = serde_json::from_slice::<Command>(&bytes)?;
    if bytes.len() > MAX_REQUEST_FRAME && !matches!(&command, Command::MarkdownEdit { .. }) { bail!("frame_too_large") }
    Ok(command)
}
