//! poing-node: Poing's GPU jobs (analysis, takes, remixes, a new song's
//! singing) served to a Poing on another machine, so the UI can run on a
//! Mac while the Windows PC does the work. The jobs are the app's own
//! (`audio::run_job`); this is the serving around them.
//!
//! It is a Makepad AI hub node in all but its jobs: a self-signed TLS
//! certificate (`<home>/tls`), the hub's TLS front checking every request's
//! fleet credential (`<home>/fleet/fleet.key`, `revoked`), and a LAN beacon
//! in fleet `poing`. Behind the front, a loopback server holds each client's
//! song bundles in `<home>/bundles/<client>/<bundle id>/` and runs one job
//! at a time (one GPU).
//!
//!   poing-node [serve] [--port 8125] [--listen 0.0.0.0] [--no-beacon]
//!   poing-node pair [name] [--days N]   a credential for a new machine
//!   poing-node revoke <name>
//!   poing-node join <credential> [host:port]   (on the Mac) keep the credential, use that node
//!   poing-node find                            the nodes on this LAN (their beacons)
//!
//! `<home>`: `POING_NODE_HOME`, else `%LOCALAPPDATA%\Poing\node` (Windows),
//! `~/Library/Application Support/Poing/node` (macOS), `~/.local/share/poing-node`.

use crate::audio;
use crate::remote::{self, DEFAULT_PORT, FLEET};
use crate::wire::{self, FileEntry, JobRequest, WireEvent};
use makepad_ai_hub::fleet_auth::{self, now_secs, FleetKey, Role, Verifier};
use makepad_ai_hub::front::{self, FrontConfig};
use makepad_micro_serde::*;
use makepad_network::tls::{self, TlsIdentity, TlsServer};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often a working job says it is alive.
const PING: Duration = Duration::from_secs(3);
/// The biggest file a client may send (a long song's WAV is a few hundred MB).
const MAX_UPLOAD: u64 = 2 << 30;

pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let result = match args.first().map(String::as_str) {
        None => serve(&[]),
        Some("serve") => serve(rest),
        Some(a) if a.starts_with("--") => serve(&args),
        Some("pair") => pair(rest),
        Some("revoke") => revoke(rest),
        Some("join") => join(rest),
        Some("find") => find(),
        _ => Err("usage: poing-node [serve [--port N] [--listen ADDR] [--no-beacon]] | pair [name] [--days N] | revoke <name> | join <credential> [host:port] | find".into()),
    };
    if let Err(e) = result {
        eprintln!("poing-node: {e}");
        std::process::exit(1);
    }
}

pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("POING_NODE_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    let user = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| user.join("AppData").join("Local")).join("Poing").join("node")
    } else if cfg!(target_os = "macos") {
        user.join("Library").join("Application Support").join("Poing").join("node")
    } else {
        user.join(".local").join("share").join("poing-node")
    }
}

fn machine_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "the Windows PC".into())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// The fleet key, made on first use.
fn fleet_key(fleet: &Path) -> Result<FleetKey, String> {
    let path = fleet.join(fleet_auth::FLEET_KEY);
    if path.exists() {
        return FleetKey::load(&path).map_err(|e| format!("{}: {e}", path.display()));
    }
    let key = FleetKey::generate().map_err(|e| e.to_string())?;
    key.save(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(key)
}

/// Where other machines reach this one: its interfaces (unix), else the
/// address its default route leaves from (Windows; nothing is sent).
fn addresses(port: u16) -> Vec<String> {
    let mut ips: Vec<std::net::Ipv4Addr> = makepad_core_util::netif::interfaces().into_iter().map(|i| i.addr).filter(|a| !a.is_loopback()).collect();
    if ips.is_empty() {
        let routed = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect("192.0.2.1:9").and(s.local_addr()));
        if let Ok(SocketAddr::V4(a)) = routed {
            ips.push(*a.ip());
        }
    }
    ips.into_iter().map(|ip| format!("{ip}:{port}")).collect()
}

// ------------------------------------------------------------------ commands

fn pair(args: &[String]) -> Result<(), String> {
    let name = args.first().filter(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| "mac".into());
    let days: u64 = flag(args, "--days").map(|d| d.parse().map_err(|_| "--days: a number")).transpose()?.unwrap_or(5 * 365);
    let home = home();
    let key = fleet_key(&home.join("fleet"))?;
    let credential = key.issue(&name, Role::Lan, now_secs() + days * 86_400).map_err(|e| e.to_string())?;
    let identity = TlsIdentity::load_or_create(&home.join("tls"), "poing-node").map_err(|e| e.to_string())?;
    let port = DEFAULT_PORT;
    println!("A credential for \"{name}\" (keep it like a password; revoke it with: poing-node revoke {name}):\n\n  {credential}\n");
    println!("On the Mac, from the poing checkout (after cargo build --release -p poing):\n");
    for a in addresses(port) {
        println!("  target/release/poing-node join {credential} {a}");
    }
    println!("\nThis PC's certificate (the Mac records it on first contact): {}", identity.fingerprint_hex());
    Ok(())
}

fn revoke(args: &[String]) -> Result<(), String> {
    let name = args.first().ok_or("revoke whom? poing-node revoke <name>")?;
    let path = home().join("fleet").join(fleet_auth::REVOKED);
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.lines().any(|l| l.trim() == name) {
        text.push_str(&format!("{name}\n"));
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    println!("Revoked {name}. Restart poing-node for it to take effect.");
    Ok(())
}

/// On the client: keep the credential, and use the node when given.
fn join(args: &[String]) -> Result<(), String> {
    let credential = args.first().ok_or("join with what? poing-node join <credential> [host:port]")?;
    let path = remote::pair(credential)?;
    println!("Credential kept in {}", path.display());
    if let Some(address) = args.get(1) {
        let target = remote::parse_target(address).ok_or_else(|| format!("{address}: not host:port"))?;
        remote::set_target(target.clone()).map_err(|e| format!("could not save the target: {e}"))?;
        if let remote::Target::Node(a) = &target {
            match remote::check(a) {
                Ok(name) => println!("Poing now runs its jobs on {name} ({a})."),
                Err(e) => println!("Poing will run its jobs on {a}, but it does not answer yet: {e}"),
            }
        }
    }
    Ok(())
}

/// List the nodes whose beacons arrive within a few seconds.
fn find() -> Result<(), String> {
    remote::discovered();
    std::thread::sleep(Duration::from_secs(5));
    let nodes = remote::discovered();
    if nodes.is_empty() {
        println!("No poing-node heard on this network (beacons are UDP broadcasts on port 41830). Give its address to join instead.");
    }
    for a in nodes {
        println!("{a}");
    }
    Ok(())
}

fn serve(args: &[String]) -> Result<(), String> {
    let port: u16 = match flag(args, "--port").or_else(|| std::env::var("POING_NODE_PORT").ok()) {
        Some(p) => p.parse().map_err(|_| format!("{p}: not a port"))?,
        None => DEFAULT_PORT,
    };
    let listen: std::net::IpAddr = flag(args, "--listen").unwrap_or_else(|| "0.0.0.0".into()).parse().map_err(|_| "--listen: an IP address")?;
    let home = home();
    kill_children_with_me();
    let fleet = home.join("fleet");
    fleet_key(&fleet)?;
    let identity = TlsIdentity::load_or_create(&home.join("tls"), "poing-node").map_err(|e| format!("tls identity: {e}"))?;
    let fingerprint = identity.fingerprint;
    let verifier = Verifier::load(&fleet, fingerprint)?;
    let inner = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let inner_addr = inner.local_addr().map_err(|e| e.to_string())?;
    let mut secret = [0u8; 24];
    tls::os_random(&mut secret).map_err(|e| e.to_string())?;
    let secret = tls::to_hex(&secret);
    front::start_front(FrontConfig {
        listen: SocketAddr::new(listen, port),
        inner: inner_addr,
        tls: TlsServer::new(identity).map_err(|e| format!("tls: {e}"))?,
        verifier: Arc::new(verifier),
        front_secret: secret.clone(),
        edge: false,
    })
    .map_err(|e| format!("port {port}: {e}"))?;
    if !args.iter().any(|a| a == "--no-beacon") {
        makepad_ai_hub::discovery::start_fleet_beacon(makepad_ai_hub::discovery::mint_node_id(), port, FLEET.into(), tls::to_hex(&fingerprint[..16]));
    }
    println!("poing-node {} on {} (pid {}), serving {}", env!("CARGO_PKG_VERSION"), machine_name(), std::process::id(), home.display());
    println!("  listening on {listen}:{port}; reach it at {}", addresses(port).join(", "));
    println!("  certificate {}", tls::to_hex(&fingerprint));
    println!("  pair a machine: poing-node pair <name>");
    let home = Arc::new(home);
    for conn in inner.incoming() {
        let Ok(conn) = conn else { continue };
        let (home, secret) = (home.clone(), secret.clone());
        let _ = std::thread::Builder::new().name("poing-node-conn".into()).spawn(move || {
            if let Err(e) = handle(conn, &home, &secret) {
                eprintln!("poing-node: {e}");
            }
        });
    }
    Ok(())
}

// ---------------------------------------------------------- the inner server

struct Request {
    method: String,
    target: String,
    length: u64,
    client: String,
}

fn respond(out: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    };
    write!(out, "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())?;
    out.write_all(body)?;
    out.flush()
}

fn error(out: &mut TcpStream, status: u16, message: &str) -> Result<(), String> {
    let body = format!("{{\"error\":{}}}", message.serialize_json());
    respond(out, status, "application/json", body.as_bytes()).map_err(|e| e.to_string())
}

fn handle(conn: TcpStream, home: &Path, secret: &str) -> Result<(), String> {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(120)));
    let mut out = conn.try_clone().map_err(|e| e.to_string())?;
    let mut reader = BufReader::with_capacity(1 << 16, conn);
    let mut lines = Vec::new();
    let mut size = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Ok(());
        }
        size += line.len();
        if size > 16 * 1024 {
            return error(&mut out, 400, "request head too large");
        }
        let l = line.trim_end().to_string();
        if l.is_empty() {
            break;
        }
        lines.push(l);
    }
    // only requests that came through the front (and its credential check)
    let Some(caller) = front::caller_from_headers(&lines, secret) else {
        return error(&mut out, 403, "requests come through the TLS front");
    };
    let mut parts = lines.first().map(|l| l.split(' ')).into_iter().flatten();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let length = lines.iter().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<u64>().ok()).flatten()
    });
    let req = Request { method, target, length: length.unwrap_or(0), client: caller.client_id };
    route(req, &mut reader, &mut out, home)
}

fn read_body(req: &Request, reader: &mut dyn Read, max: u64) -> Result<Vec<u8>, String> {
    if req.length > max {
        return Err(format!("{} bytes is too much", req.length));
    }
    let mut body = Vec::with_capacity(req.length as usize);
    reader.take(req.length).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() as u64 != req.length {
        return Err("the request was cut off".into());
    }
    Ok(body)
}

fn route(req: Request, reader: &mut dyn Read, out: &mut TcpStream, home: &Path) -> Result<(), String> {
    let path = req.target.split('?').next().unwrap_or("").to_string();
    if (req.method.as_str(), path.as_str()) == ("GET", "/health") {
        let busy = GPU.try_lock().is_err();
        let body = format!("{{\"service\":\"poing-node\",\"name\":{},\"version\":{},\"busy\":{busy}}}", machine_name().serialize_json(), env!("CARGO_PKG_VERSION").serialize_json());
        return respond(out, 200, "application/json", body.as_bytes()).map_err(|e| e.to_string());
    }
    let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let ["bundle", id, action] = segs.as_slice() else { return error(out, 404, "no such route") };
    if !wire::valid_bundle_id(id) {
        return error(out, 400, "bad bundle id");
    }
    let bundle = home.join("bundles").join(&req.client).join(id);
    std::fs::create_dir_all(&bundle).map_err(|e| e.to_string())?;
    match (req.method.as_str(), *action) {
        ("POST", "have") => {
            let body = read_body(&req, reader, 16 << 20)?;
            let Ok(wanted) = Vec::<FileEntry>::deserialize_json(&String::from_utf8_lossy(&body)) else { return error(out, 400, "bad manifest") };
            let missing = wire::missing(&bundle, &wanted);
            respond(out, 200, "application/json", wire::line(&missing).as_bytes()).map_err(|e| e.to_string())
        }
        ("POST", "put") => {
            let (Some(rel), Some(sha)) = (wire::query(&req.target, "path"), wire::query(&req.target, "sha")) else { return error(out, 400, "put needs path and sha") };
            if req.length > MAX_UPLOAD {
                return error(out, 413, "file too big");
            }
            let body = read_body(&req, reader, MAX_UPLOAD)?;
            match wire::store(&bundle, &rel, Some(&sha), &body) {
                Ok(()) => respond(out, 200, "application/json", b"{\"ok\":true}").map_err(|e| e.to_string()),
                Err(e) => error(out, 400, &e),
            }
        }
        ("GET", "get") => {
            let Some(file) = wire::query(&req.target, "path").and_then(|r| wire::safe_rel(&r)).map(|r| bundle.join(r)) else { return error(out, 400, "bad path") };
            let Ok(mut f) = std::fs::File::open(&file) else { return error(out, 404, "no such file") };
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            write!(out, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
            std::io::copy(&mut (&mut f).take(len), out).map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
        }
        ("POST", "job") => {
            let body = read_body(&req, reader, 16 << 20)?;
            let Ok(request) = JobRequest::deserialize_json(&String::from_utf8_lossy(&body)) else { return error(out, 400, "bad job") };
            write!(out, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
            run_job(request, &bundle, out);
            Ok(())
        }
        _ => error(out, 404, "no such route"),
    }
}

/// One job at a time: the GPU is shared.
static GPU: Mutex<()> = Mutex::new(());

/// The client's end of a job's stream (a client that went away stops
/// getting lines; the job still finishes).
struct Lines<'a> {
    out: &'a mut TcpStream,
    open: bool,
}

impl Lines<'_> {
    fn send(&mut self, event: &WireEvent) {
        if self.open && self.out.write_all(wire::line(event).as_bytes()).and_then(|_| self.out.flush()).is_err() {
            self.open = false;
        }
    }
}

/// The files changed since `sent`, and those the job made and removed
/// since the `start` of the job, as a `Sync` (then they count as sent).
fn sync(bundle: &Path, start: &BTreeMap<String, wire::Stamp>, sent: &mut BTreeMap<String, wire::Stamp>, lines: &mut Lines) {
    let now = wire::scan(bundle);
    let files: Vec<FileEntry> = wire::changed(sent, &now).iter().filter_map(|rel| wire::entry(bundle, rel).ok()).collect();
    let gone = wire::gone(start, sent, &now);
    *sent = now;
    if !files.is_empty() || !gone.is_empty() {
        lines.send(&WireEvent::Sync { files, gone });
    }
}

fn run_job(request: JobRequest, bundle: &Path, out: &mut TcpStream) {
    let mut lines = Lines { out, open: true };
    let gpu = loop {
        match GPU.try_lock() {
            Ok(g) => break g,
            Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                lines.send(&WireEvent::Starting { message: "Waiting for another job on this PC".into(), progress: 0.0 });
                if !lines.open {
                    return;
                }
                std::thread::sleep(PING);
            }
        }
    };
    let job = match wire::job_on_node(request.job, bundle, request.sr) {
        Ok(j) => j,
        Err(e) => {
            lines.send(&WireEvent::Failed { message: format!("The Windows PC could not take this job: {e}"), in_bundle: true });
            lines.send(&WireEvent::End { at: now_secs() });
            return;
        }
    };
    let start = wire::scan(bundle);
    let mut sent = start.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::Builder::new().name("poing-node-job".into()).spawn(move || audio::run_job(job, &tx));
    let Ok(worker) = worker else {
        lines.send(&WireEvent::Failed { message: "The Windows PC could not start the job".into(), in_bundle: true });
        lines.send(&WireEvent::End { at: now_secs() });
        return;
    };
    loop {
        match rx.recv_timeout(PING) {
            Ok(update) => {
                let Some(event) = wire::outbound_event(update, bundle) else { continue };
                if event.needs_files() {
                    sync(bundle, &start, &mut sent, &mut lines);
                }
                lines.send(&event);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => lines.send(&WireEvent::Ping { at: now_secs() }),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if worker.join().is_err() {
        lines.send(&WireEvent::Failed { message: "The job crashed on the Windows PC".into(), in_bundle: true });
    }
    drop(gpu);
    sync(bundle, &start, &mut sent, &mut lines);
    lines.send(&WireEvent::End { at: now_secs() });
}

/// Every process the node starts (uv, Python, ACE-Step, the engine) ends
/// with it: a node that is killed must not leave a model holding the GPU.
#[cfg(windows)]
fn kill_children_with_me() {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct BasicLimit {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimit {
        basic: BasicLimit,
        io_counters: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(job: *mut c_void, class: i32, info: *mut c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
    }
    const EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const KILL_ON_JOB_CLOSE: u32 = 0x2000;
    // SAFETY: plain Win32 calls with a correctly laid out
    // JOBOBJECT_EXTENDED_LIMIT_INFORMATION; the job handle stays open for
    // the process's life (closing it at exit is what kills the children).
    unsafe {
        let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if job.is_null() {
            return eprintln!("poing-node: no job object; child processes may outlive the node");
        }
        let mut info = ExtendedLimit::default();
        info.basic.limit_flags = KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(job, EXTENDED_LIMIT_INFORMATION, &mut info as *mut _ as *mut c_void, std::mem::size_of::<ExtendedLimit>() as u32) != 0
            && AssignProcessToJobObject(job, GetCurrentProcess()) != 0;
        if !ok {
            eprintln!("poing-node: could not join a job object; child processes may outlive the node");
        }
    }
}

#[cfg(not(windows))]
fn kill_children_with_me() {}
