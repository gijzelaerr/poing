//! Where GPU jobs run: here, or on a poing-node (Gijs's Windows PC) while
//! the UI runs on a Mac. The job worker asks [`route`] for every job; with a
//! node chosen, analysis, takes, remixes and a new song's singing run there
//! and their files come back into the local bundle (see `wire.rs`). Render,
//! Claude's writers and writing a new song's sheet always run here.
//!
//! The node is reached the AI hub's way: TLS with its self-signed
//! certificate recorded in `~/.makepad/ai-hub/known_nodes`, and each
//! request carrying a proof of this machine's fleet credential bound to that
//! certificate (`makepad_ai_hub::fleet_auth`). The credential comes from
//! `poing-node pair` on the PC: `POING_NODE_CREDENTIAL`, else
//! `~/.makepad/ai-hub/poing.credential` (not the hub's `client.credential`:
//! that one is for another fleet's key).
//!
//! The target: `POING_NODE` (`host:port`, or `local`), else what Settings
//! chose ([`set_target`], kept in Poing's config folder as `node.txt`),
//! else local.

// Settings (main session) calls the target/pairing API; poing-node the rest.
#![allow(dead_code)]

use crate::audio::{self, load_stem, send, Job, JobUpdate};
use crate::wire::{self, FileEntry, Inbound, JobRequest, WireEvent};
use makepad_ai_hub::fleet_auth::{self, Credential};
use makepad_micro_serde::*;
use makepad_network::tls::KnownHosts;
use makepad_network::SocketStream;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The node's TLS port unless told otherwise (the AI hub uses 8123).
pub const DEFAULT_PORT: u16 = 8125;
/// The fleet name in a node's LAN beacon.
pub const FLEET: &str = "poing";
pub const CREDENTIAL_FILE: &str = "poing.credential";
const TARGET_FILE: &str = "node.txt";
/// Every node this machine has sent its jobs to, one `host:port` per line.
const USED_FILE: &str = "nodes.txt";

fn used_nodes() -> Vec<String> {
    let text = config_dir().and_then(|d| std::fs::read_to_string(d.join(USED_FILE)).ok()).unwrap_or_default();
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}
/// The node pings every few seconds while it works: this much silence means it is gone.
const SILENCE: Duration = Duration::from_secs(20);
const CONNECT: Duration = Duration::from_secs(3);
pub const UNREACHABLE: &str = "The Windows PC is not reachable";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Jobs run on this machine.
    Local,
    /// Jobs run on the node at `host:port`.
    Node(String),
}

/// A node Settings can offer.
#[derive(Clone, Debug)]
pub struct NodeInfo {
    pub address: String,
    /// Heard on the LAN just now (else: the configured one).
    pub discovered: bool,
}

/// What the status line and Settings show.
#[derive(Clone, Debug)]
pub struct Status {
    pub target: Target,
    /// A credential is in place.
    pub paired: bool,
    /// The node's machine name, once it answered.
    pub node_name: Option<String>,
    /// Why the last remote job could not run, if it could not.
    pub last_error: Option<String>,
}

#[derive(Default)]
struct State {
    target: Option<Target>,
    node_name: Option<String>,
    last_error: Option<String>,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static STATE: Mutex<State> = Mutex::new(State { target: None, node_name: None, last_error: None });
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Poing's settings folder (as main.rs's `config_dir`).
fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join(if cfg!(target_os = "linux") { "poing" } else { "Poing" }))
}

/// `host`, `host:port` or `local` as a target (the port defaults to [`DEFAULT_PORT`]).
pub fn parse_target(text: &str) -> Option<Target> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if t.eq_ignore_ascii_case("local") || t == "0" {
        return Some(Target::Local);
    }
    let t = t.trim_start_matches("https://").trim_end_matches('/');
    if t.contains(['/', ' ', '\\']) {
        return None;
    }
    // host:port, [v6]:port, or a bare host
    let has_port = match t.rsplit_once(':') {
        Some((host, port)) => port.parse::<u16>().is_ok() && (!host.contains(':') || host.ends_with(']')),
        None => false,
    };
    Some(Target::Node(if has_port { t.to_string() } else { format!("{t}:{DEFAULT_PORT}") }))
}

/// Where jobs run now.
pub fn target() -> Target {
    if let Some(t) = std::env::var("POING_NODE").ok().and_then(|v| parse_target(&v)) {
        return t;
    }
    let mut st = state();
    if st.target.is_none() {
        let saved = config_dir().and_then(|d| std::fs::read_to_string(d.join(TARGET_FILE)).ok()).and_then(|t| parse_target(&t));
        st.target = Some(saved.unwrap_or(Target::Local));
    }
    st.target.clone().unwrap_or(Target::Local)
}

/// Choose where jobs run (kept for the next start; `POING_NODE` still wins).
pub fn set_target(target: Target) -> Result<(), String> {
    let dir = config_dir().ok_or("no settings folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = match &target {
        Target::Local => "local".to_string(),
        Target::Node(a) => a.clone(),
    };
    std::fs::write(dir.join(TARGET_FILE), format!("{text}\n")).map_err(|e| e.to_string())?;
    // a node used once stays on Settings' list after switching to This PC
    if let Target::Node(a) = &target {
        let mut used = used_nodes();
        if !used.contains(a) {
            used.push(a.clone());
            let _ = std::fs::write(dir.join(USED_FILE), used.join("\n") + "\n");
        }
    }
    let mut st = state();
    st.target = Some(target);
    st.node_name = None;
    st.last_error = None;
    Ok(())
}

/// The nodes to offer: the configured one and those heard on the LAN.
/// The first call starts listening for beacons.
pub fn targets() -> Vec<NodeInfo> {
    let mut out: Vec<NodeInfo> = discovered().into_iter().map(|address| NodeInfo { address, discovered: true }).collect();
    let mut known = used_nodes();
    if let Target::Node(a) = target() {
        known.retain(|n| n != &a);
        known.insert(0, a);
    }
    for (i, a) in known.into_iter().enumerate() {
        if !out.iter().any(|n| n.address == a) {
            out.insert(i.min(out.len()), NodeInfo { address: a, discovered: false });
        }
    }
    out
}

pub fn status() -> Status {
    let target = target();
    let st = state();
    Status { target, paired: credential().is_some(), node_name: st.node_name.clone(), last_error: st.last_error.clone() }
}

/// One line for the status bar, when jobs run elsewhere.
pub fn status_line() -> Option<String> {
    let s = status();
    let Target::Node(address) = &s.target else { return None };
    let name = s.node_name.clone().unwrap_or_else(|| address.clone());
    Some(match (&s.last_error, s.paired) {
        (_, false) => format!("Jobs run on {name}, but this machine is not paired (poing-node pair)"),
        (Some(e), _) => format!("Jobs run on {name}: {e}"),
        (None, _) => format!("Jobs run on {name}"),
    })
}

/// This machine's credential for the node.
pub fn credential() -> Option<Credential> {
    std::env::var("POING_NODE_CREDENTIAL")
        .ok()
        .or_else(|| std::fs::read_to_string(fleet_auth::client_dir().join(CREDENTIAL_FILE)).ok())
        .and_then(|t| Credential::parse(&t))
}

/// Keep the credential `poing-node pair` printed (`mkc2.…`).
pub fn pair(text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    Credential::parse(text).ok_or("That is not a node credential (it starts with mkc2.)")?;
    let dir = fleet_auth::client_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(CREDENTIAL_FILE);
    makepad_network::tls::write_private(&path, format!("{text}\n").as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    state().last_error = None;
    Ok(path)
}

// ---------------------------------------------------------------- discovery

fn discovery() -> &'static Mutex<HashMap<u64, (String, Instant)>> {
    static SET: OnceLock<Mutex<HashMap<u64, (String, Instant)>>> = OnceLock::new();
    SET.get_or_init(|| {
        // one listener per process, sharing the port with other Makepad apps
        let _ = std::thread::Builder::new().name("poing-discovery".into()).spawn(|| {
            let Ok(socket) = makepad_core_util::udp::bind_reuse_udp(makepad_ai_hub::discovery::DISCOVERY_PORT) else { return };
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = socket.recv_from(&mut buf) {
                let Some((node_id, address)) = std::str::from_utf8(&buf[..n]).ok().and_then(|t| beacon_address(t, from.ip())) else { continue };
                discovery().lock().unwrap_or_else(|e| e.into_inner()).insert(node_id, (address, Instant::now()));
            }
        });
        Mutex::new(HashMap::new())
    })
}

/// A poing-node beacon's node id and `host:port` (other fleets' beacons are not ours).
pub fn beacon_address(text: &str, from: std::net::IpAddr) -> Option<(u64, String)> {
    use makepad_ai_hub::discovery::{normalize_fleet, FleetBeaconJson, FLEET_SERVICE};
    let b = FleetBeaconJson::deserialize_json(text).ok()?;
    if b.service != FLEET_SERVICE || normalize_fleet(&b.fleet) != FLEET || b.port == 0 {
        return None;
    }
    Some((b.node_id, match from {
        std::net::IpAddr::V6(v6) => format!("[{v6}]:{}", b.port),
        v4 => format!("{v4}:{}", b.port),
    }))
}

/// Nodes whose beacon was heard in the last 15 s.
pub fn discovered() -> Vec<String> {
    let mut set = discovery().lock().unwrap_or_else(|e| e.into_inner());
    set.retain(|_, (_, seen)| seen.elapsed() < Duration::from_secs(15));
    let mut out: Vec<String> = set.values().map(|(a, _)| a.clone()).collect();
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------- the client

/// Why a remote job stopped early.
#[derive(Debug, PartialEq)]
pub enum Lost {
    /// The node did not answer, or went silent (the message says which).
    Unreachable(String),
    /// The node answered and said no.
    Refused(String),
}

impl Lost {
    fn message(&self) -> String {
        match self {
            Lost::Unreachable(_) => UNREACHABLE.to_string(),
            Lost::Refused(why) => why.clone(),
        }
    }
}

/// A node and this machine's credential for it.
pub struct Node {
    host: String,
    port: String,
    credential: Credential,
}

pub struct Response {
    pub status: u16,
    pub length: Option<u64>,
    pub body: BufReader<SocketStream>,
}

impl Response {
    pub fn bytes(mut self) -> Result<Vec<u8>, Lost> {
        let mut out = Vec::new();
        let r = match self.length {
            Some(n) => (&mut self.body).take(n).read_to_end(&mut out),
            None => self.body.read_to_end(&mut out),
        };
        r.map_err(|e| Lost::Unreachable(e.to_string()))?;
        if self.length.is_some_and(|n| out.len() as u64 != n) {
            return Err(Lost::Unreachable("the answer was cut off".into()));
        }
        Ok(out)
    }

    fn error_text(self) -> String {
        let status = self.status;
        let body = self.bytes().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let msg = makepad_micro_serde::JsonValue::deserialize_json(&body).ok().and_then(|v| v.key("error").and_then(|e| e.as_str().map(String::from)));
        msg.unwrap_or_else(|| format!("the Windows PC answered {status}"))
    }
}

impl Node {
    pub fn new(address: &str) -> Result<Node, Lost> {
        let credential = credential().ok_or_else(|| Lost::Refused("This machine is not paired with the Windows PC: run poing-node pair there".into()))?;
        let (host, port) = address.rsplit_once(':').ok_or_else(|| Lost::Refused(format!("{address}: not host:port")))?;
        Ok(Node { host: host.trim_start_matches('[').trim_end_matches(']').to_string(), port: port.to_string(), credential })
    }

    fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// One request: TLS to the node (its certificate recorded, ssh-style),
    /// the credential's proof for that certificate, `Connection: close`.
    pub fn request(&self, method: &str, target: &str, body: &[u8], timeout: Duration) -> Result<Response, Lost> {
        let address = self.address();
        // a quick TCP check first: a dead address must not hang the job
        if !makepad_ai_hub::discovery::answers(&address, CONNECT) {
            return Err(Lost::Unreachable(format!("{address} does not answer")));
        }
        let (mut stream, fp) = SocketStream::connect_capture(&self.host, &self.port).map_err(|e| Lost::Unreachable(format!("tls {address}: {e}")))?;
        KnownHosts::new(fleet_auth::client_dir().join(fleet_auth::KNOWN_NODES)).observe(&address, &fp);
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
        let head = format!(
            "{method} {target} HTTP/1.1\r\nHost: {address}\r\nAuthorization: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.credential.authorization(&fp),
            body.len()
        );
        let io = |e: std::io::Error| Lost::Unreachable(e.to_string());
        stream.write_all(head.as_bytes()).map_err(io)?;
        for chunk in body.chunks(1 << 20) {
            stream.write_all(chunk).map_err(io)?;
        }
        stream.flush().map_err(io)?;
        let mut body = BufReader::with_capacity(1 << 16, stream);
        let mut line = String::new();
        body.read_line(&mut line).map_err(io)?;
        let status = line.split(' ').nth(1).and_then(|s| s.parse::<u16>().ok()).ok_or_else(|| Lost::Unreachable("no answer".into()))?;
        let mut length = None;
        loop {
            line.clear();
            if body.read_line(&mut line).map_err(io)? == 0 {
                break;
            }
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().ok();
                }
            }
        }
        let response = Response { status, length, body };
        match status {
            200 => Ok(response),
            401 | 403 => Err(Lost::Refused(format!("The Windows PC refused this machine ({}): pair it again with poing-node pair", response.error_text()))),
            _ => Err(Lost::Refused(response.error_text())),
        }
    }

    pub fn get_json(&self, target: &str) -> Result<makepad_micro_serde::JsonValue, Lost> {
        let body = self.request("GET", target, &[], Duration::from_secs(30))?.bytes()?;
        makepad_micro_serde::JsonValue::deserialize_json(&String::from_utf8_lossy(&body)).map_err(|_| Lost::Refused("the Windows PC sent something unreadable".into()))
    }

    /// The node's machine name (and a check that it accepts this machine).
    pub fn health(&self) -> Result<String, Lost> {
        let v = self.get_json("/health")?;
        Ok(v.key("name").and_then(|n| n.as_str().map(String::from)).unwrap_or_else(|| self.address()))
    }
}

/// Check a node now (Settings' test button): its name, or why not.
pub fn check(address: &str) -> Result<String, String> {
    let name = Node::new(address).and_then(|n| n.health()).map_err(|e| match e {
        Lost::Unreachable(why) => format!("{UNREACHABLE} ({why})"),
        Lost::Refused(why) => why,
    })?;
    state().node_name = Some(name.clone());
    Ok(name)
}

/// The worker's question for each job: run it here (Some), or it ran on the node (None).
pub(crate) fn route(job: Job, tx: &Sender<JobUpdate>) -> Option<Job> {
    let Target::Node(address) = target() else { return Some(job) };
    match job {
        // Claude writes the sheet here; the node sings it
        Job::Song { prompt, dir, count, sr } => {
            let (bundle, seeds) = audio::prepare_song(&prompt, &dir, count, tx)?;
            let sing = Job::Sing { bundle, count, seeds, sr };
            let outbound = wire::outbound(&sing)?;
            run(&address, outbound, true, tx);
            None
        }
        job => match wire::outbound(&job) {
            Some(outbound) => {
                run(&address, outbound, false, tx);
                None
            }
            None => Some(job),
        },
    }
}

/// Run one job on the node; every way it can end sends something (a
/// failure says why), so the worker's JobDone always follows.
fn run(address: &str, (bundle, sr, outbound): (PathBuf, u32, wire::Outbound), new_song: bool, tx: &Sender<JobUpdate>) {
    // a new song's failure belongs to it only once it opened
    let mut owner = if new_song { PathBuf::new() } else { bundle.clone() };
    let result = Node::new(address).and_then(|node| {
        let name = node.health()?;
        state().node_name = Some(name.clone());
        run_on(&node, &name, &bundle, sr, outbound, &mut owner, tx)
    });
    let mut st = state();
    match result {
        Ok(()) => st.last_error = None,
        Err(lost) => {
            st.last_error = Some(lost.message());
            drop(st);
            send(tx, JobUpdate::Failed { bundle: owner, message: lost.message() });
        }
    }
}

fn run_on(node: &Node, name: &str, bundle: &Path, sr: u32, outbound: wire::Outbound, owner: &mut PathBuf, tx: &Sender<JobUpdate>) -> Result<(), Lost> {
    let id = wire::bundle_id(bundle);
    let on = |m: String| format!("{m}  ·  on {name}");
    // the inputs: what the node lacks, by content
    let mut manifest = Vec::new();
    for (rel, path) in &outbound.files {
        let e = local_entry(path).map_err(|e| Lost::Refused(format!("{}: {e}", path.display())))?;
        manifest.push(FileEntry { path: rel.clone(), ..e });
    }
    let missing = node.request("POST", &format!("/bundle/{id}/have"), wire::line(&manifest).as_bytes(), Duration::from_secs(120))?.bytes()?;
    let missing = Vec::<String>::deserialize_json(&String::from_utf8_lossy(&missing)).map_err(|_| Lost::Refused("the Windows PC sent something unreadable".into()))?;
    for (i, rel) in missing.iter().enumerate() {
        let Some((_, path)) = outbound.files.iter().find(|(r, _)| r == rel) else { continue };
        let sha = manifest.iter().find(|f| &f.path == rel).map(|f| f.sha.clone()).unwrap_or_default();
        send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: on(format!("Sending {rel} ({}/{})", i + 1, missing.len())), progress: 0.0 });
        let data = std::fs::read(path).map_err(|e| Lost::Refused(format!("{}: {e}", path.display())))?;
        node.request("POST", &format!("/bundle/{id}/put?path={}&sha={sha}", wire::url_encode(rel)), &data, Duration::from_secs(120))?;
    }
    // the job: its updates as they come
    let request = JobRequest { sr, job: outbound.job };
    // the socket's own timeout is only a backstop: silence is timed on the lines
    let response = node.request("POST", &format!("/bundle/{id}/job"), wire::line(&request).as_bytes(), SILENCE * 3)?;
    let lines = line_reader(response.body);
    read_events(&lines, SILENCE, &mut |event| {
        match wire::inbound(event, bundle) {
            Inbound::Alive => {}
            Inbound::End => return Ok(false),
            Inbound::Sync(files, gone) => {
                for f in files {
                    fetch(node, &id, bundle, &f)?;
                }
                for rel in gone {
                    wire::remove(bundle, &rel);
                }
            }
            Inbound::Analyzed(stems) => {
                let mut decoded = Vec::new();
                for s in stems {
                    let path = wire::safe_rel(&s.file).map(|r| bundle.join(r)).ok_or_else(|| Lost::Refused(format!("bad stem path {}", s.file)))?;
                    match load_stem(&path, sr) {
                        Ok(stem) => decoded.push((s.id, s.file, stem)),
                        Err(e) => {
                            send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message: format!("Analysis failed: {e}") });
                            return Ok(true);
                        }
                    }
                }
                send(tx, JobUpdate::Analyzed { bundle: bundle.to_path_buf(), stems: decoded });
            }
            Inbound::TakeReady(take) => {
                let path = wire::safe_rel(&take.file).map(|r| bundle.join(r)).ok_or_else(|| Lost::Refused(format!("bad take path {}", take.file)))?;
                match load_stem(&path, sr) {
                    Ok(stem) => send(tx, JobUpdate::TakeReady { bundle: bundle.to_path_buf(), take, stem }),
                    Err(e) => send(tx, JobUpdate::Failed { bundle: owner.clone(), message: format!("Take {} failed: {e}", take.id.trim_start_matches('t')) }),
                }
            }
            Inbound::Update(u) => {
                let u = match u {
                    JobUpdate::Progress { bundle, message, progress } => JobUpdate::Progress { bundle, message: on(message), progress },
                    JobUpdate::Starting { message, progress } => JobUpdate::Starting { message: on(message), progress },
                    JobUpdate::SongReady { project } => {
                        *owner = bundle.to_path_buf();
                        JobUpdate::SongReady { project }
                    }
                    JobUpdate::Failed { bundle: b, message } => JobUpdate::Failed { bundle: if b.as_os_str().is_empty() { owner.clone() } else { b }, message },
                    u => u,
                };
                send(tx, u);
            }
        }
        Ok(true)
    })
}

/// The lines of a response, read on their own thread (whatever the TLS
/// layer does with a socket timeout, the reader times the silence).
pub fn line_reader(mut body: impl BufRead + Send + 'static) -> Receiver<Result<String, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new().name("poing-node-lines".into()).spawn(move || loop {
        let mut line = String::new();
        match body.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {
                if tx.send(Ok(line)).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e.to_string()));
                return;
            }
        }
    });
    rx
}

/// Hand the node's update lines to `on` (which says whether to go on) until
/// `End`. `silence` without a line (the node pings every few seconds), or
/// the stream ending without `End`, means the node is gone.
pub fn read_events(lines: &Receiver<Result<String, String>>, silence: Duration, on: &mut dyn FnMut(WireEvent) -> Result<bool, Lost>) -> Result<(), Lost> {
    use std::sync::mpsc::RecvTimeoutError;
    loop {
        let line = match lines.recv_timeout(silence) {
            Ok(Ok(line)) => line,
            Ok(Err(e)) => return Err(Lost::Unreachable(format!("no word from the Windows PC ({e})"))),
            Err(RecvTimeoutError::Timeout) => return Err(Lost::Unreachable(format!("no word from the Windows PC for {} s", silence.as_secs()))),
            Err(RecvTimeoutError::Disconnected) => return Err(Lost::Unreachable("the Windows PC stopped mid-job".into())),
        };
        let Ok(event) = WireEvent::deserialize_json(line.trim()) else { continue };
        if !on(event)? {
            return Ok(());
        }
    }
}

/// Bring one file the node changed into the local bundle (skipped when the
/// local copy already has that content).
fn fetch(node: &Node, id: &str, bundle: &Path, f: &FileEntry) -> Result<(), Lost> {
    let rel = wire::safe_rel(&f.path).ok_or_else(|| Lost::Refused(format!("bad path {}", f.path)))?;
    let local = bundle.join(rel);
    if local_entry(&local).is_ok_and(|e| e.sha == f.sha) {
        return Ok(());
    }
    let data = node.request("GET", &format!("/bundle/{id}/get?path={}", wire::url_encode(&f.path)), &[], Duration::from_secs(120))?.bytes()?;
    // the node may have changed it since (a log): what came is the newest
    wire::store(bundle, &f.path, None, &data).map_err(Lost::Refused)
}

/// size and sha256 of a local file, remembered by (path, size, mtime).
fn local_entry(path: &Path) -> std::io::Result<FileEntry> {
    static CACHE: Mutex<Option<HashMap<PathBuf, (u64, std::time::SystemTime, String)>>> = Mutex::new(None);
    let meta = std::fs::metadata(path)?;
    let (size, mtime) = (meta.len(), meta.modified()?);
    if let Some((s, m, sha)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(path) {
        if *s == size && *m == mtime {
            return Ok(FileEntry { path: String::new(), size, sha: sha.clone() });
        }
    }
    let sha = wire::sha256_file(path)?;
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(path.to_path_buf(), (size, mtime, sha.clone()));
    Ok(FileEntry { path: String::new(), size, sha })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        assert_eq!(parse_target("local"), Some(Target::Local));
        assert_eq!(parse_target(" 192.168.1.20:8125 "), Some(Target::Node("192.168.1.20:8125".into())));
        assert_eq!(parse_target("glacier"), Some(Target::Node(format!("glacier:{DEFAULT_PORT}"))));
        assert_eq!(parse_target("glacier.gemsbok-iguana.ts.net:9000"), Some(Target::Node("glacier.gemsbok-iguana.ts.net:9000".into())));
        assert_eq!(parse_target("https://10.0.0.2:8125/"), Some(Target::Node("10.0.0.2:8125".into())));
        assert_eq!(parse_target("[fe80::1]:8125"), Some(Target::Node("[fe80::1]:8125".into())));
        assert_eq!(parse_target(""), None);
        assert_eq!(parse_target("a b"), None);
    }

    #[test]
    fn only_poing_beacons_count() {
        use makepad_ai_hub::discovery::{FleetBeaconJson, FLEET_SERVICE};
        let beacon = |fleet: &str| FleetBeaconJson { service: FLEET_SERVICE.into(), node_id: 1, port: 8125, fleet: fleet.into(), node_key: String::new() }.serialize_json();
        let from: std::net::IpAddr = "192.168.1.20".parse().unwrap();
        assert_eq!(beacon_address(&beacon("poing"), from), Some((1, "192.168.1.20:8125".to_string())));
        assert_eq!(beacon_address(&beacon("gen"), from), None);
        assert_eq!(beacon_address("not json", from), None);
    }

    /// A node that goes quiet mid-job (its PC asleep, the cable out) ends
    /// the job after `silence`, even when the socket never times out, and a
    /// node that dies ends it at once.
    #[test]
    fn a_silent_or_dead_node_ends_the_job() {
        use std::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            // first: one ping, then silence
            let (mut a, _) = listener.accept().unwrap();
            a.write_all(wire::line(&WireEvent::Ping { at: 1 }).as_bytes()).unwrap();
            // second: a progress line, then the process dies (the socket closes)
            let (mut b, _) = listener.accept().unwrap();
            b.write_all(wire::line(&WireEvent::Progress { message: "Singing".into(), progress: 0.5 }).as_bytes()).unwrap();
            drop(b);
            std::thread::sleep(Duration::from_secs(2));
            drop(a);
        });
        // no socket timeout at all: the reader alone notices
        let quiet = line_reader(BufReader::new(TcpStream::connect(addr).unwrap()));
        let mut seen = Vec::new();
        let t0 = Instant::now();
        let r = read_events(&quiet, Duration::from_millis(300), &mut |e| {
            seen.push(format!("{e:?}"));
            Ok(true)
        });
        assert!(matches!(r, Err(Lost::Unreachable(_))), "{r:?}");
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert_eq!(seen.len(), 1);
        let dead = line_reader(BufReader::new(TcpStream::connect(addr).unwrap()));
        let r = read_events(&dead, Duration::from_secs(5), &mut |_| Ok(true));
        assert_eq!(r, Err(Lost::Unreachable("the Windows PC stopped mid-job".into())));
        assert_eq!(Lost::Unreachable(String::new()).message(), UNREACHABLE);
        server.join().unwrap();
    }

    #[test]
    fn end_stops_reading() {
        let text = format!("{}garbage\n{}{}", wire::line(&WireEvent::Ping { at: 1 }), wire::line(&WireEvent::End { at: 2 }), wire::line(&WireEvent::Ping { at: 3 }));
        let mut n = 0;
        let r = read_events(&line_reader(std::io::Cursor::new(text.into_bytes())), Duration::from_secs(5), &mut |e| {
            n += 1;
            Ok(!matches!(e, WireEvent::End { .. }))
        });
        assert_eq!((r, n), (Ok(()), 2));
    }
}
