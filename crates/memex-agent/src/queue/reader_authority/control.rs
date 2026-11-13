//! Isolated authenticated-channel and authority-fence implementation.
//! Compiled only for tests until provisioning and production transport are approved.
use anyhow::{ensure, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    collections::HashSet,
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::Mutex,
};
use zeroize::Zeroizing;

const FRAME_MAX: usize = 65536;
const LEASE_MS: u64 = 5000;
const RENEW_MS: u64 = 1000;

fn nonce() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| anyhow::anyhow!("random_unavailable"))?;
    Ok(hex::encode(bytes))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    boot: String,
    publisher: String,
    channel: u64,
    key_id: String,
    library: String,
    principal: String,
    session: String,
    grant_epoch: u64,
    target: String,
    scope: String,
    ceiling: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Challenge {
        challenge: String,
    },
    Renew {
        challenge: String,
        remaining_ms: u64,
    },
    Revoke,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    binding: Binding,
    sequence: u64,
    message: Message,
}

/// Direction is part of the MAC domain, not a caller-selected payload field.
#[derive(Clone, Copy)]
enum Direction {
    Publisher,
    Daemon,
}
impl Direction {
    fn domain(self) -> &'static [u8] {
        match self {
            Self::Publisher => b"memex.reader.control.v1.publisher-to-daemon",
            Self::Daemon => b"memex.reader.control.v1.daemon-to-publisher",
        }
    }
}

struct Channel {
    key: Zeroizing<[u8; 32]>,
    binding: Binding,
    direction: Direction,
    sequence: u64,
}
impl Channel {
    fn new(key: [u8; 32], binding: Binding, direction: Direction) -> Self {
        Self {
            key: Zeroizing::new(key),
            binding,
            direction,
            sequence: 0,
        }
    }
    fn mac(&self, payload: &[u8]) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.key.as_ref()).unwrap();
        let domain = self.direction.domain();
        mac.update(&(domain.len() as u32).to_be_bytes());
        mac.update(domain);
        mac.update(&(payload.len() as u32).to_be_bytes());
        mac.update(payload);
        mac
    }
    fn encode(&mut self, message: Message) -> Result<Vec<u8>> {
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("sequence_exhausted"))?;
        let payload = serde_json::to_vec(&Envelope {
            binding: self.binding.clone(),
            sequence,
            message,
        })?;
        ensure!(payload.len() + 36 <= FRAME_MAX, "invalid_control_frame");
        let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&self.mac(&payload).finalize().into_bytes());
        self.sequence = sequence;
        Ok(bytes)
    }
    fn receive(&mut self, input: &mut impl Read) -> Result<Message> {
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        ensure!(
            length > 0 && length <= FRAME_MAX - 36,
            "invalid_control_frame"
        );
        let mut payload = vec![0; length];
        input.read_exact(&mut payload)?;
        let mut tag = [0; 32];
        input.read_exact(&mut tag)?;
        // Mac::verify_slice uses constant-time tag comparison. Parse only afterwards.
        self.mac(&payload)
            .verify_slice(&tag)
            .map_err(|_| anyhow::anyhow!("control_authentication_failed"))?;
        let envelope: Envelope = serde_json::from_slice(&payload)?;
        ensure!(
            envelope.binding == self.binding && envelope.sequence > self.sequence,
            "stale_control_frame"
        );
        self.sequence = envelope.sequence;
        Ok(envelope.message)
    }

    /// Any channel/authentication/protocol failure terminates this epoch.
    fn update(
        &mut self,
        input: &mut impl Read,
        fence: &Fence,
        now: impl FnOnce() -> Result<u64>,
    ) -> Result<()> {
        let result = (|| {
            ensure!(
                matches!(self.direction, Direction::Publisher),
                "invalid_control_direction"
            );
            match self.receive(input)? {
                Message::Renew {
                    challenge,
                    remaining_ms,
                } => fence.renew(now, &challenge, remaining_ms),
                Message::Revoke => {
                    fence.revoke();
                    Ok(())
                }
                Message::Challenge { .. } => anyhow::bail!("invalid_control_operation"),
            }
        })();
        if result.is_err() {
            fence.revoke();
        }
        result
    }
}

/// Includes time spent asleep; never substitutes wall time or std::Instant.
#[cfg(target_os = "macos")]
fn continuous_ms() -> Result<u64> {
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    let mut info = Timebase { numer: 0, denom: 0 };
    ensure!(
        unsafe { mach_timebase_info(&mut info) } == 0 && info.denom != 0,
        "continuous_clock_unavailable"
    );
    let ns = u128::from(unsafe { mach_continuous_time() }) * u128::from(info.numer)
        / u128::from(info.denom);
    Ok(u64::try_from(ns / 1_000_000)?)
}
#[cfg(not(target_os = "macos"))]
fn continuous_ms() -> Result<u64> {
    anyhow::bail!("continuous_clock_unavailable")
}

/// Freeze original lifetime once. Wall rollback never replenishes it.
struct GrantDeadline {
    continuous: std::cell::Cell<u64>,
    wall: u64,
    revoked: std::cell::Cell<bool>,
}
impl GrantDeadline {
    fn new(now: u64, wall_now: u64, wall_expiry: u64) -> Result<Self> {
        let remaining = wall_expiry
            .checked_sub(wall_now)
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow::anyhow!("expired_grant"))?;
        Ok(Self {
            continuous: std::cell::Cell::new(
                now.checked_add(remaining)
                    .ok_or_else(|| anyhow::anyhow!("invalid_deadline"))?,
            ),
            wall: wall_expiry,
            revoked: std::cell::Cell::new(false),
        })
    }
    fn remaining(
        &self,
        now: u64,
        wall: u64,
        grant: bool,
        channel: bool,
        trust: bool,
    ) -> Result<u64> {
        if !(grant && channel && trust) {
            self.revoked.set(true);
        }
        ensure!(!self.revoked.get(), "authority_lost");
        let remaining = self
            .continuous
            .get()
            .saturating_sub(now)
            .min(self.wall.saturating_sub(wall));
        if remaining == 0 {
            self.revoked.set(true);
            anyhow::bail!("expired_grant")
        }
        // An observed forward wall jump can shorten, never lengthen, the grant.
        self.continuous.set(now + remaining);
        Ok(remaining.min(LEASE_MS))
    }
}

struct State {
    pending: Option<(String, u64)>,
    deadline: Option<u64>,
    revoked: bool,
    jobs: HashSet<String>,
    sent: HashSet<(String, u64)>,
}
struct Fence {
    state: Mutex<State>,
}
impl Fence {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                pending: None,
                deadline: None,
                revoked: false,
                jobs: HashSet::new(),
                sent: HashSet::new(),
            }),
        }
    }
    fn challenge(&self, now: u64) -> Result<String> {
        let mut s = self.state.lock().unwrap();
        ensure!(!s.revoked, "authority_lost");
        if s.deadline.is_some_and(|d| now >= d) {
            s.revoked = true;
            anyhow::bail!("expired_lease")
        }
        if let Some((_, t0)) = &s.pending {
            ensure!(now >= t0.saturating_add(LEASE_MS), "challenge_pending");
        }
        let id = nonce()?;
        s.pending = Some((id.clone(), now));
        Ok(id)
    }
    /// Call only after authenticated Channel::receive and exact Binding validation.
    fn renew(
        &self,
        clock: impl FnOnce() -> Result<u64>,
        challenge: &str,
        remaining: u64,
    ) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let now = clock()?;
        ensure!(!s.revoked, "authority_lost");
        ensure!(remaining > 0 && remaining <= LEASE_MS, "invalid_lease");
        let (id, t0) = s
            .pending
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("stale_challenge"))?;
        ensure!(id == challenge, "stale_challenge");
        let deadline = t0
            .checked_add(remaining)
            .ok_or_else(|| anyhow::anyhow!("invalid_deadline"))?;
        ensure!(
            now < deadline && !s.deadline.is_some_and(|d| now >= d),
            "expired_lease"
        );
        s.pending = None;
        s.deadline = Some(deadline);
        Ok(())
    }
    fn revoke(&self) {
        let mut s = self.state.lock().unwrap();
        s.revoked = true;
        s.pending = None;
        s.jobs.clear();
    }
    fn bind(&self, now: u64, job: &str) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        Self::check(&s, now)?;
        s.jobs.insert(job.into());
        Ok(())
    }
    fn check(s: &State, now: u64) -> Result<()> {
        ensure!(
            !s.revoked && s.deadline.is_some_and(|d| now < d),
            "authority_lost"
        );
        Ok(())
    }
    fn commit<T>(
        &self,
        job: &str,
        now: impl Fn() -> Result<u64>,
        current: impl FnOnce() -> Result<()>,
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let s = self.state.lock().unwrap();
        Self::check(&s, now()?)?;
        ensure!(s.jobs.contains(job), "job_not_authorized");
        current()?;
        Self::check(&s, now()?)?;
        // The Store-internal final publication action runs before this guard drops.
        publish()
    }
    /// Fixture transport is an actual nonblocking Unix syscall, not TLS buffering.
    /// Production HTTP/TLS remains unconnected until its own handoff is proven.
    fn send(
        &self,
        job: &str,
        attempt: u64,
        now: impl Fn() -> Result<u64>,
        current: impl FnOnce() -> Result<()>,
        socket: &mut UnixStream,
        bytes: &[u8],
    ) -> Result<usize> {
        use std::os::fd::AsRawFd;
        let flags = unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_GETFL) };
        ensure!(
            flags >= 0 && flags & libc::O_NONBLOCK != 0,
            "blocking_transport_forbidden"
        );
        let mut s = self.state.lock().unwrap();
        Self::check(&s, now()?)?;
        ensure!(
            s.jobs.contains(job) && !s.sent.contains(&(job.into(), attempt)),
            "dispatch_not_authorized"
        );
        ensure!(!bytes.is_empty(), "empty_request");
        current()?;
        Self::check(&s, now()?)?;
        // No async queue or userspace TLS buffer: write() is the handoff boundary.
        let count = socket.write(bytes)?;
        ensure!(count > 0, "zero_handoff");
        s.sent.insert((job.into(), attempt));
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
