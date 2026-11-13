//! Length-prefixed (u32 big-endian) JSON frames over the owner-only agent socket.

use crate::ClientError;
use std::{
    io::{ErrorKind, Read, Write},
    os::unix::{fs::FileTypeExt, fs::MetadataExt, net::UnixStream},
    path::Path,
    time::{Duration, Instant},
};

pub const MAX_RESPONSE: usize = 256 * 1024;

pub struct Framing;

impl Framing {
    pub fn encode(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len() + 4);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }
}

/// The socket must be a socket, owned by this user, mode exactly 0600, single link.
pub fn is_safe_owner_socket(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
    let euid = unsafe { libc::geteuid() };
    meta.file_type().is_socket() && meta.uid() == euid && meta.nlink() == 1 && meta.mode() & 0o777 == 0o600
}

pub(crate) fn send(
    socket: &Path,
    request: &[u8],
    request_maximum: usize,
    maximum: usize,
    timeout: Duration,
) -> Result<Vec<u8>, ClientError> {
    if request.len() > request_maximum {
        return Err(ClientError::TooLarge);
    }
    if std::fs::symlink_metadata(socket).is_err() {
        return Err(ClientError::Unavailable);
    }
    if !is_safe_owner_socket(socket) {
        return Err(ClientError::UnsafeSocket);
    }
    let deadline = Instant::now() + timeout;
    let mut stream = UnixStream::connect(socket).map_err(|_| ClientError::Unavailable)?;
    // Set once: macOS rejects SO_RCVTIMEO with EINVAL after the peer has closed, so the
    // timeouts cannot be refreshed per read. The loops below still honour the deadline.
    stream.set_read_timeout(Some(timeout)).map_err(map_io)?;
    stream.set_write_timeout(Some(timeout)).map_err(map_io)?;
    write_all(&mut stream, &Framing::encode(request), deadline)?;
    let mut header = [0u8; 4];
    read_exact(&mut stream, &mut header, deadline)?;
    let length = u32::from_be_bytes(header) as usize;
    if length > maximum {
        return Err(ClientError::TooLarge);
    }
    let mut body = vec![0u8; length];
    read_exact(&mut stream, &mut body, deadline)?;
    Ok(body)
}

fn remaining(deadline: Instant) -> Result<Duration, ClientError> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(ClientError::Timeout)
    } else {
        Ok(left)
    }
}

fn map_io(error: std::io::Error) -> ClientError {
    match error.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => ClientError::Timeout,
        ErrorKind::UnexpectedEof => ClientError::InvalidFrame,
        _ => ClientError::Unavailable,
    }
}

fn write_all(stream: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> Result<(), ClientError> {
    while !bytes.is_empty() {
        remaining(deadline)?;
        match stream.write(bytes) {
            Ok(0) => return Err(ClientError::Unavailable),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(map_io(e)),
        }
    }
    Ok(())
}

fn read_exact(stream: &mut UnixStream, mut buffer: &mut [u8], deadline: Instant) -> Result<(), ClientError> {
    while !buffer.is_empty() {
        remaining(deadline)?;
        match stream.read(buffer) {
            Ok(0) => return Err(ClientError::InvalidFrame),
            Ok(n) => buffer = &mut buffer[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(map_io(e)),
        }
    }
    Ok(())
}
