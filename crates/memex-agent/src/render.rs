//! Opens one public HTTPS page in the local Chrome and snapshots its DOM.
//!
//! Contract (`SourceJob::render_page`):
//! - Only `https` URLs on the default port, without credentials or fragment.
//!   The host must resolve only to global addresses before Chrome starts.
//! - Chrome runs `--headless=new` with a fresh temporary `--user-data-dir`
//!   that is deleted afterwards. No existing profile, cookie or credential is
//!   used, read or exported. Nothing is clicked, no login is attempted, and a
//!   verification page is returned as is (admission rejects it as a challenge).
//! - Every network request Chrome makes (navigation, redirects, subresources,
//!   WebSockets) goes through a validating local exit: only `CONNECT` to port
//!   443 whose host resolves exclusively to global addresses, pinned to the
//!   validated address. Plain HTTP, other ports and private or reserved
//!   addresses are refused. Direct DNS in Chrome is disabled, as are QUIC,
//!   non-proxied WebRTC UDP, images, extensions, sync and downloads.
//! - The final document URL must equal the requested URL; otherwise the
//!   capture fails with `page_changed` and the other page is not kept.
//! - Bounded by `RENDER_DEADLINE`, `MAX_RENDERED_HTML`, `MAX_TUNNELS` and
//!   `MAX_EXIT_BYTES`; on timeout the whole Chrome process group is killed and
//!   the failure is retryable (`fetch_retry`).
//! - Reason codes: `chrome_unavailable` (no Chrome at the configured or
//!   standard path), `page_changed`, `address_not_public`,
//!   `network_policy_rejected`, `unsupported_content` (oversized document),
//!   `fetch_retry` (timeout or navigation failure).
use crate::fetch::{is_global, Resolver, MAX_BODY, MAX_RESOLVED_ADDRESSES};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use url::Url;

pub const RENDER_DEADLINE: Duration = Duration::from_secs(45);
pub const MAX_RENDERED_HTML: usize = MAX_BODY;
pub const MAX_TUNNELS: usize = 32;
pub const MAX_EXIT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CDP_MESSAGE: usize = 4 * MAX_RENDERED_HTML;

/// The serialized main document of one rendered page.
pub struct RenderedDocument {
    pub html: Vec<u8>,
    pub final_url: String,
}

/// Loads `url` and returns its serialized DOM. Every request the renderer
/// makes must be admitted by `resolver` and `is_global`, as `ChromeRenderer` does.
pub trait PageRenderer: Send + Sync {
    fn render(
        &self,
        url: &Url,
        resolver: &dyn Resolver,
        deadline: Instant,
    ) -> Result<RenderedDocument>;
}

/// Standard macOS install locations, checked in order.
const STANDARD_CHROME: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
];

pub struct ChromeRenderer {
    configured: Option<PathBuf>,
}

impl ChromeRenderer {
    pub fn new(configured: Option<PathBuf>) -> Self {
        Self { configured }
    }
    fn executable(&self) -> Result<PathBuf> {
        let mut candidates: Vec<PathBuf> = match &self.configured {
            Some(path) => vec![path.clone()],
            None => STANDARD_CHROME.iter().map(PathBuf::from).collect(),
        };
        if self.configured.is_none() {
            if let Some(home) = std::env::var_os("HOME") {
                candidates.push(
                    Path::new(&home)
                        .join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
                );
            }
        }
        candidates
            .into_iter()
            .find(|path| path.is_file())
            .context("chrome_unavailable")
    }
}

impl PageRenderer for ChromeRenderer {
    fn render(
        &self,
        url: &Url,
        resolver: &dyn Resolver,
        deadline: Instant,
    ) -> Result<RenderedDocument> {
        let executable = self.executable()?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let profile = tempfile::Builder::new()
            .prefix("render-profile-")
            .tempdir()?;
        let stop = AtomicBool::new(false);
        let exit = ExitBudget::default();
        std::thread::scope(|scope| {
            scope.spawn(|| serve_exit(scope, &listener, resolver, &stop, &exit));
            let result = run_chrome(&executable, profile.path(), port, url, deadline);
            stop.store(true, Ordering::SeqCst);
            result
        })
    }
}

#[derive(Default)]
struct ExitBudget {
    tunnels: AtomicUsize,
    bytes: AtomicU64,
}

/// The only network exit Chrome has. Runs until `stop`.
fn serve_exit<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    listener: &'scope TcpListener,
    resolver: &'scope dyn Resolver,
    stop: &'scope AtomicBool,
    exit: &'scope ExitBudget,
) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((client, _)) => {
                if exit.tunnels.fetch_add(1, Ordering::SeqCst) >= MAX_TUNNELS {
                    exit.tunnels.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                scope.spawn(move || {
                    let _ = tunnel(client, resolver, stop, exit);
                    exit.tunnels.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Parses `CONNECT host:443 HTTP/1.1` and returns the validated, pinned target.
pub(crate) fn admit_connect(head: &str, resolver: &dyn Resolver) -> Result<SocketAddr> {
    let line = head.lines().next().unwrap_or_default();
    let mut parts = line.split(' ');
    let (Some("CONNECT"), Some(authority), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("url_policy_rejected")
    };
    anyhow::ensure!(version.starts_with("HTTP/1."), "url_policy_rejected");
    let url = Url::parse(&format!("https://{authority}/"))
        .map_err(|_| anyhow::anyhow!("url_malformed"))?;
    anyhow::ensure!(
        url.port_or_known_default() == Some(443) && authority.ends_with(":443"),
        "url_port_forbidden"
    );
    let addresses = match url.host().context("host_missing")? {
        url::Host::Ipv4(ip) => vec![IpAddr::V4(ip)],
        url::Host::Ipv6(ip) => vec![IpAddr::V6(ip)],
        url::Host::Domain(host) => resolver.resolve(host, Duration::from_secs(5))?,
    };
    if addresses.is_empty()
        || addresses.len() > MAX_RESOLVED_ADDRESSES
        || addresses.iter().any(|address| !is_global(*address))
    {
        bail!("address_not_public")
    }
    Ok(SocketAddr::new(addresses[0], 443))
}

fn tunnel(
    mut client: TcpStream,
    resolver: &dyn Resolver,
    stop: &AtomicBool,
    exit: &ExitBudget,
) -> Result<()> {
    client.set_nonblocking(false)?;
    client.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < 8192, "url_policy_rejected");
        anyhow::ensure!(client.read(&mut byte)? == 1, "transport_error");
        head.push(byte[0]);
    }
    let target = match admit_connect(&String::from_utf8_lossy(&head), resolver) {
        Ok(target) => target,
        Err(error) => {
            let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
            return Err(error);
        }
    };
    let upstream = match TcpStream::connect_timeout(&target, Duration::from_secs(10)) {
        Ok(upstream) => upstream,
        Err(error) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
            return Err(error.into());
        }
    };
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    let (mut client_read, mut upstream_read) = (client.try_clone()?, upstream.try_clone()?);
    std::thread::scope(|scope| {
        scope.spawn(|| pump(&mut client_read, &upstream, stop, exit));
        pump(&mut upstream_read, &client, stop, exit);
    });
    Ok(())
}

fn pump(from: &mut TcpStream, to: &TcpStream, stop: &AtomicBool, exit: &ExitBudget) {
    let _ = from.set_read_timeout(Some(Duration::from_millis(200)));
    let mut to = to;
    let mut buffer = [0u8; 16 * 1024];
    while !stop.load(Ordering::SeqCst) {
        match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if exit.bytes.fetch_add(n as u64, Ordering::SeqCst) + n as u64 > MAX_EXIT_BYTES
                    || to.write_all(&buffer[..n]).is_err()
                {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    let _ = from.shutdown(std::net::Shutdown::Both);
    let _ = to.shutdown(std::net::Shutdown::Both);
}

/// Chrome with `--remote-debugging-pipe`: it reads CDP from fd 3 and writes fd 4.
fn run_chrome(
    executable: &Path,
    profile: &Path,
    exit_port: u16,
    url: &Url,
    deadline: Instant,
) -> Result<RenderedDocument> {
    use std::os::unix::{io::FromRawFd, process::CommandExt};
    let (to_chrome, chrome_reads) = pipe()?;
    let (chrome_writes, from_chrome) = pipe()?;
    let (child_in, child_out) = (chrome_reads, chrome_writes);
    let mut command = std::process::Command::new(executable);
    command
        .arg("--headless=new")
        .arg("--remote-debugging-pipe")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--proxy-server=http://127.0.0.1:{exit_port}"))
        .arg("--proxy-bypass-list=<-loopback>")
        .arg("--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1")
        .arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
        .arg("--disable-quic")
        .arg("--blink-settings=imagesEnabled=false")
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-sync",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-default-apps",
            "--disable-breakpad",
            "--password-store=basic",
            "--use-mock-keychain",
            "--mute-audio",
            "--renderer-process-limit=2",
            "about:blank",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env_clear()
        .env("HOME", profile)
        .process_group(0);
    unsafe {
        command.pre_exec(move || {
            // Move both ends above 4 first so dup2 never targets its own source.
            let input = libc::fcntl(child_in, libc::F_DUPFD, 10);
            let output = libc::fcntl(child_out, libc::F_DUPFD, 10);
            if input < 0 || output < 0 || libc::dup2(input, 3) < 0 || libc::dup2(output, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("chrome_unavailable"))?;
    unsafe {
        libc::close(child_in);
        libc::close(child_out);
    }
    let pid = child.id() as i32;
    let writer = unsafe { std::fs::File::from_raw_fd(to_chrome) };
    let reader = unsafe { std::fs::File::from_raw_fd(from_chrome) };
    let (messages, inbox) = mpsc::channel();
    std::thread::spawn(move || read_cdp(reader, messages));
    let result = Cdp {
        writer,
        inbox,
        backlog: Default::default(),
        next: 0,
        deadline,
    }
    .snapshot(url);
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.wait();
    result
}

fn pipe() -> Result<(i32, i32)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        bail!("render_unavailable")
    }
    for fd in fds {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok((fds[1], fds[0]))
}

fn read_cdp(mut reader: std::fs::File, messages: mpsc::Sender<Value>) {
    let mut pending = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    while let Ok(n) = reader.read(&mut buffer) {
        if n == 0 {
            return;
        }
        pending.extend_from_slice(&buffer[..n]);
        while let Some(end) = pending.iter().position(|byte| *byte == 0) {
            let message: Vec<u8> = pending.drain(..=end).collect();
            let Ok(value) = serde_json::from_slice::<Value>(&message[..end]) else {
                return;
            };
            if messages.send(value).is_err() {
                return;
            }
        }
        if pending.len() > MAX_CDP_MESSAGE {
            return;
        }
    }
}

struct Cdp {
    writer: std::fs::File,
    inbox: mpsc::Receiver<Value>,
    /// Events that arrived while waiting for a command reply.
    backlog: std::collections::VecDeque<Value>,
    next: u64,
    deadline: Instant,
}

impl Cdp {
    fn receive(&self) -> Result<Value> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!remaining.is_zero(), "fetch_deadline");
        self.inbox
            .recv_timeout(remaining)
            .map_err(|_| anyhow::anyhow!("fetch_deadline"))
    }
    fn call(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = session.into();
        }
        let mut bytes = serde_json::to_vec(&message)?;
        bytes.push(0);
        self.writer
            .write_all(&bytes)
            .map_err(|_| anyhow::anyhow!("transport_error"))?;
        loop {
            let reply = self.receive()?;
            if reply["id"].as_u64() == Some(id) {
                anyhow::ensure!(reply.get("error").is_none(), "transport_error");
                return Ok(reply["result"].clone());
            }
            if reply.get("method").is_some() {
                self.backlog.push_back(reply);
            }
        }
    }
    fn wait_event(&mut self, session: &str, method: &str) -> Result<()> {
        loop {
            let event = match self.backlog.pop_front() {
                Some(event) => event,
                None => self.receive()?,
            };
            if event["method"] == method && event["sessionId"] == session {
                return Ok(());
            }
        }
    }
    fn snapshot(mut self, url: &Url) -> Result<RenderedDocument> {
        self.call(
            None,
            "Browser.setDownloadBehavior",
            json!({ "behavior": "deny" }),
        )?;
        let target = self.call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
        let target = target["targetId"]
            .as_str()
            .context("transport_error")?
            .to_owned();
        let session = self.call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )?;
        let session = session["sessionId"]
            .as_str()
            .context("transport_error")?
            .to_owned();
        self.call(Some(&session), "Page.enable", json!({}))?;
        let navigation = self.call(
            Some(&session),
            "Page.navigate",
            json!({ "url": url.as_str() }),
        )?;
        anyhow::ensure!(navigation.get("errorText").is_none(), "transport_error");
        self.wait_event(&session, "Page.loadEventFired")?;
        let value = self.call(
            Some(&session),
            "Runtime.evaluate",
            json!({
                "expression": "JSON.stringify([location.href, document.documentElement ? document.documentElement.outerHTML : ''])",
                "returnByValue": true,
            }),
        )?;
        let value = value["result"]["value"]
            .as_str()
            .context("transport_error")?;
        let (final_url, html): (String, String) =
            serde_json::from_str(value).map_err(|_| anyhow::anyhow!("transport_error"))?;
        anyhow::ensure!(html.len() <= MAX_RENDERED_HTML, "body_too_large");
        Ok(RenderedDocument {
            html: html.into_bytes(),
            final_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Vec<IpAddr>);
    impl Resolver for Fixed {
        fn resolve(&self, _: &str, _: Duration) -> Result<Vec<IpAddr>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn exit_admits_only_connect_to_public_port_443() {
        let public = Fixed(vec!["93.184.216.34".parse().unwrap()]);
        let connect =
            |authority: &str| format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
        assert_eq!(
            admit_connect(&connect("site.example:443"), &public).unwrap(),
            "93.184.216.34:443".parse().unwrap()
        );
        for (head, code) in [
            (connect("site.example:8443"), "url_port_forbidden"),
            (connect("127.0.0.1:443"), "address_not_public"),
            (connect("[::1]:443"), "address_not_public"),
            (connect("10.0.0.1:443"), "address_not_public"),
            (
                "GET http://site.example/ HTTP/1.1\r\n\r\n".into(),
                "url_policy_rejected",
            ),
        ] {
            assert_eq!(
                admit_connect(&head, &public).unwrap_err().to_string(),
                code,
                "{head}"
            );
        }
        // A name is refused if any of its addresses is not global (rebinding).
        let mixed = Fixed(vec![
            "93.184.216.34".parse().unwrap(),
            "192.168.1.2".parse().unwrap(),
        ]);
        assert_eq!(
            admit_connect(&connect("site.example:443"), &mixed)
                .unwrap_err()
                .to_string(),
            "address_not_public"
        );
    }

    #[test]
    fn missing_chrome_is_a_named_failure() {
        let renderer = ChromeRenderer::new(Some("/nonexistent/chrome".into()));
        let error = renderer
            .render(
                &Url::parse("https://site.example/").unwrap(),
                &Fixed(vec![]),
                Instant::now() + Duration::from_secs(1),
            )
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "chrome_unavailable");
    }
}
