// perseids: run a repo's GitHub Actions jobs on fresh Boat boxes, one box per job and matrix entry, all at once.
// The Rust port of bin/perseids: same commands, flags, environment, box names, board, summary and exit codes.
// Planning runs here (src/plan.rs); each box still runs lib/runner.rb under Ruby, embedded below.
mod plan;

use serde_json::{json, Value as J};
use std::fs;
use std::io::{BufRead, IsTerminal};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RUNNER: &str = include_str!("../../lib/runner.rb");
const BASE_SH: &str = include_str!("../../images/base.sh");
const USAGE: &str = "\
perseids: run a repo's GitHub Actions jobs on fresh Boat boxes (boat.dev), one box per job and per matrix entry,
all at once. Each box gets the exact commit (a git bundle, with history), runs the job's steps with lib/runner.rb,
sends its log back, and is deleted on every way out. No GitHub Actions minutes and no push.

  perseids key                                                           # save your Boat API key (asked once, never shown)
  perseids run [job...] [-w workflow] [-C dir] [--keep-going] [--dirty]   # no job: every job, in needs: order
  perseids exec '<command>' [-C dir] [--dirty]                           # any command, on one box
  perseids image build [-C dir]                                          # bake .perseids/setup.sh into an image
  perseids ls                                                            # this account's perseids boxes
  perseids reap [--all]                                                  # delete orphaned (or all) boxes from here

Exit: 0 passed, 1 failed, 2 incomplete (something it can't run on a box was skipped), 99 a box itself failed.
Settings (environment):
  BOAT_API_KEY, or PERSEIDS_KEY_FILE (default ~/.config/perseids/boat.env, written by `perseids key`).
    The key goes to curl on stdin and is never printed or sent to a box.
  PERSEIDS_TYPE   box size: small | default | large | xlarge (default large: 8 vCPU / 16 GB)
  PERSEIDS_MAX    boxes at once across this account's perseids runs (default 10)
  PERSEIDS_TTL    seconds before a box stops itself, the safety net (default 7200)
  PERSEIDS_TAIL   log lines to print for each failed job (default 40)
  PERSEIDS_EVENT  the github.event_name jobs see (default push; workflow_dispatch runs manual-only steps)
";

// --- process-wide state, so a signal or a die anywhere still deletes every box ---
static API: OnceLock<(String, String)> = OnceLock::new(); // base URL, key or hosted token
static LIVE: Mutex<Vec<String>> = Mutex::new(Vec::new()); // boxes that exist
static KIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new()); // ssh and scp children
static STOP: AtomicBool = AtomicBool::new(false);
static WORK: OnceLock<PathBuf> = OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn say(m: impl std::fmt::Display) {
    eprintln!("perseids: {m}");
}
fn die(m: impl std::fmt::Display) -> ! {
    say(m);
    finish(99)
}
fn finish(code: i32) -> ! {
    cleanup();
    std::process::exit(code)
}
fn cleanup() {
    STOP.store(true, Ordering::SeqCst);
    for p in std::mem::take(&mut *lock(&KIDS)) {
        unsafe { libc::kill(p as i32, libc::SIGTERM) };
    }
    for b in std::mem::take(&mut *lock(&LIVE)) {
        destroy(&b);
    }
    if let Some(w) = WORK.get() {
        let _ = fs::remove_dir_all(w);
    }
}
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty()) // bash ${X:-default}: empty counts as unset
}
fn home() -> String {
    env("HOME").unwrap_or_default()
}
fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
fn secs(n: u64) {
    sleep(Duration::from_secs(n))
}
/// A single-quoted shell word.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
/// tr -c 'A-Za-z0-9<keep>' '-' | cut -c1-n
fn slug(s: &str, n: usize, keep: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || keep.as_bytes().contains(&b) { b as char } else { '-' }).take(n).collect()
}
fn first_word(s: &str) -> &str {
    s.split(' ').next().unwrap_or("")
}
fn chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn strftime(fmt: &str, utc: bool) -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if utc { libc::gmtime_r(&t, &mut tm) } else { libc::localtime_r(&t, &mut tm) };
        let mut buf = [0u8; 64];
        let f = std::ffi::CString::new(fmt).unwrap();
        let n = libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), f.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}
fn hostname() -> String {
    let mut b = [0u8; 256];
    unsafe { libc::gethostname(b.as_mut_ptr() as *mut libc::c_char, b.len()) };
    let s = String::from_utf8_lossy(&b[..b.iter().position(|c| *c == 0).unwrap_or(b.len())]).into_owned();
    s.split('.').next().unwrap_or("").to_string()
}
/// kill -0, except a process we may not signal still counts as alive.
fn alive(pid: &str) -> bool {
    pid.parse::<i32>().is_ok_and(|p| p > 0 && (unsafe { libc::kill(p, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)))
}
/// First `NAME=` line of an env file, like sed -n 's/^NAME=//p' | head -1.
fn env_file(path: &str, name: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let p = format!("{name}=");
    text.lines().find_map(|l| l.strip_prefix(&p).map(str::to_string))
}
/// Writes a secret file, mode 600.
fn write_secret(path: &str, text: &str) -> std::io::Result<()> {
    if let Some(d) = Path::new(path).parent() {
        fs::create_dir_all(d)?;
    }
    fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    fs::write(path, text)
}

// --- HTTP: curl -sS --retry 3 --retry-all-errors --max-time 120, the body back whatever the status ---
fn request(method: &str, url: &str, body: Option<&str>, headers: &[(&str, &str)]) -> Result<String, String> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| ureq::AgentBuilder::new().timeout(Duration::from_secs(120)).build());
    let mut wait = 1;
    for attempt in 0..=3 {
        let mut req = agent.request(method, url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let res = match body {
            Some(b) => req.send_string(b),
            None => req.call(),
        };
        match res {
            Ok(r) => return r.into_string().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(code, r)) if attempt == 3 || ![408, 429, 500, 502, 503, 504].contains(&code) => {
                return Ok(r.into_string().unwrap_or_default())
            }
            Err(ureq::Error::Transport(t)) if attempt == 3 => return Err(t.to_string()),
            Err(_) => {}
        }
        secs(wait);
        wait *= 2;
    }
    unreachable!()
}

/// Boat's API (or the hosted server's Boat-shaped subset). The key rides in a header, never argv.
fn api(method: &str, path: &str, body: Option<&str>, hdr: Option<(&str, &str)>) -> Result<String, String> {
    let (base, key) = API.get().ok_or("no key")?;
    let auth = format!("Bearer {key}");
    let mut h = vec![("Authorization", auth.as_str()), ("Content-Type", "application/json")];
    h.extend(hdr);
    request(method, &format!("{base}{path}"), body, &h)
}
fn j(s: &str) -> J {
    serde_json::from_str(s).unwrap_or(J::Null)
}
/// jq -r: strings bare, null empty.
fn js(v: &J) -> String {
    match v {
        J::String(s) => s.clone(),
        J::Null => String::new(),
        x => x.to_string(),
    }
}

// --- boxes ---

/// id, state, name for this account's perseids boxes that still run.
fn boxes() -> Result<Vec<(String, String, String)>, String> {
    let r = j(&api("GET", "/sandboxes?limit=100", None, None)?);
    Ok(r["sandboxes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| js(&b["name"]).starts_with("perseids:") && js(&b["state"]) != "archived")
        .map(|b| (js(&b["id"]), js(&b["state"]), js(&b["name"])))
        .collect())
}

fn destroy(id: &str) {
    lock(&LIVE).retain(|b| b != id);
    if api("DELETE", &format!("/sandboxes/{id}"), None, Some(("X-Ascii-Confirm-Delete", id))).is_err() {
        say(format!("box {id} wasn't deleted; delete it in Boat"));
    }
}

/// Deletes boxes started from this machine whose perseids process is gone (a killed agent, a crash).
fn reap(all: bool) -> Result<(), String> {
    let tag = format!("#{}:", hostname());
    for (id, _, name) in boxes()? {
        if name.contains(&tag) && (all || !alive(name.rsplit(':').next().unwrap_or(""))) {
            say(format!("deleting box {id} ({name})"));
            destroy(&id);
        }
    }
    Ok(())
}

fn state(id: &str) -> String {
    match api("GET", &format!("/sandboxes/{id}"), None, None) {
        Ok(r) => {
            let v = j(&r);
            match &v["sandbox"]["state"] {
                J::String(s) => s.clone(),
                _ => format!("err:{}", if v["code"].is_null() { "?".into() } else { js(&v["code"]) }),
            }
        }
        Err(e) => format!("err:{e}"),
    }
}

fn wait_state(id: &str, want: &[&str]) -> Result<(), String> {
    let mut s = String::new();
    for _ in 0..120 {
        s = state(id);
        if want.contains(&s.as_str()) {
            return Ok(());
        }
        if s == "error" || s.starts_with("err:") {
            return Err(format!("box {id} is {s}"));
        }
        secs(5);
    }
    Err(format!("box {id} stayed {s}"))
}

/// Starts a box, names it and waits for it. `slot` holds its id from the moment it exists, so the caller deletes it
/// on every way out. Boat limits starts per minute, so a refused start waits and retries.
fn create(body: &str, name: &str, slot: &mut Option<String>) -> Result<String, String> {
    let mut id = String::new();
    for i in 1..=12 {
        let idem = format!("perseids-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()) % 32768);
        let r = api("POST", "/sandboxes", Some(body), Some(("Idempotency-Key", &idem)))?;
        let v = j(&r);
        id = js(&v["sandbox"]["id"]);
        if !id.is_empty() {
            break;
        }
        let why = ["message", "error", "code"].iter().map(|k| js(&v[k])).find(|m| !m.is_empty() && m != "false");
        if v["retry"] == J::Bool(false) {
            return Err(why.unwrap_or_default());
        }
        if i == 12 {
            return Err(format!("Boat refused the box: {}", chars(&why.unwrap_or(r), 300)));
        }
        secs(10);
    }
    lock(&LIVE).push(id.clone());
    *slot = Some(id.clone());
    if STOP.load(Ordering::SeqCst) {
        return Err("stopping".into());
    }
    let _ = api("PATCH", &format!("/sandboxes/{id}"), Some(&json!({ "name": name }).to_string()), None);
    wait_state(&id, &["ready", "idle", "running"])?;
    Ok(id)
}

/// Runs a child we can kill on the way out. Callers set its stdio.
fn sh(cmd: &mut Command) -> Result<Output, String> {
    let child = cmd.spawn().map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))?;
    let pid = child.id();
    lock(&KIDS).push(pid);
    let out = child.wait_with_output();
    lock(&KIDS).retain(|p| *p != pid);
    out.map_err(|e| e.to_string())
}

struct Ssh {
    opts: Vec<String>,
    port: String,
    dest: String,
}

impl Ssh {
    fn ssh(&self, remote: &str) -> Command {
        let mut c = Command::new("ssh");
        c.args(&self.opts).args(["-p", &self.port, &self.dest, remote]).stdin(Stdio::null());
        c
    }
    fn scp(&self, files: &[PathBuf], to: &str) -> Command {
        let mut c = Command::new("scp");
        c.arg("-q").args(&self.opts).args(["-P", &self.port]).args(files).arg(format!("{}:{to}", self.dest)).stdin(Stdio::null());
        c
    }
}

/// A fresh key for this box only, its host key pinned, and a check that it answers.
fn ssh_to(id: &str, d: &Path) -> Result<Ssh, String> {
    let key = d.join("key");
    let out = sh(Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(&key).stdin(Stdio::null()).stdout(Stdio::null()))?;
    if !out.status.success() {
        return Err("ssh-keygen failed".into());
    }
    let pubkey = fs::read_to_string(d.join("key.pub")).map_err(|e| e.to_string())?;
    let body = json!({ "key": pubkey.trim_end_matches('\n') }).to_string();
    let mut v = J::Null;
    for _ in 0..20 {
        v = j(&api("POST", &format!("/sandboxes/{id}/sshkey"), Some(&body), None)?);
        if v["ok"] == J::Bool(true) {
            break;
        }
        secs(3);
    }
    if v["ok"] != J::Bool(true) {
        return Err(format!("SSH on {id} wasn't allowed"));
    }
    let known = d.join("known_hosts");
    fs::write(&known, format!("{id} {}\n", js(&v["hostKey"]))).map_err(|e| e.to_string())?;
    let ep = js(&v["sshEndpoint"]);
    let (host, port) = match ep.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None if !ep.is_empty() => (ep, "22".into()),
        None => (js(&v["machineIp"]), "22".into()),
    };
    let mut opts: Vec<String> = vec!["-i".into(), key.display().to_string()];
    for o in ["IdentitiesOnly=yes", "StrictHostKeyChecking=yes", &format!("UserKnownHostsFile={}", known.display()), &format!("HostKeyAlias={id}"), "ConnectTimeout=20", "LogLevel=ERROR", "ServerAliveInterval=30", "ServerAliveCountMax=10"] {
        opts.extend(["-o".into(), o.to_string()]);
    }
    let s = Ssh { opts, port, dest: format!("user@{host}") };
    for _ in 0..20 {
        if sh(s.ssh("true").stdout(Stdio::null()).stderr(Stdio::null())).is_ok_and(|o| o.status.success()) {
            return Ok(s);
        }
        secs(3);
    }
    Err(format!("SSH to {id} didn't answer"))
}

fn exit_code(s: std::process::ExitStatus) -> i32 {
    s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
}

// --- a run: one box per plan row ---

#[derive(Default, Clone)]
struct Slot {
    state: Option<String>, // queued|booting|running|pass|fail|incomplete|error|skip|unsupported, then a note
    start: Option<u64>,
    end: Option<u64>,
    seen: String,
}

struct Run {
    cmd: String,
    args: Vec<String>,
    rows: Vec<plan::Row>,
    slots: Mutex<Vec<Slot>>,
    work: PathBuf,
    logdir: PathBuf,
    dir: PathBuf,
    wf: String,
    prep: String,
    head: String, // the summary's first line
    name: String, // box name prefix: perseids:REPO@TAG
    suffix: String, // #HOST:PID
    ty: String,
    ttl: u64,
    max: usize,
}

fn price(ty: &str) -> f64 {
    match ty {
        "small" => 0.018,
        "default" => 0.036,
        "xlarge" => 0.20,
        _ => 0.072,
    }
}

impl Run {
    fn id(&self, i: usize) -> String {
        format!("{}-{}", self.rows[i].job, self.rows[i].index)
    }
    fn set(&self, i: usize, st: &str) {
        lock(&self.slots)[i].state = Some(st.to_string());
    }
    fn state(&self, i: usize) -> String {
        lock(&self.slots)[i].state.clone().unwrap_or_default()
    }

    fn worker(&self, i: usize) {
        let mut bx = None;
        let r = self.work_box(i, &mut bx);
        if let Some(b) = bx {
            destroy(&b);
        }
        self.set(i, &r.unwrap_or_else(|e| format!("error {e}")));
    }

    fn work_box(&self, i: usize, bx: &mut Option<String>) -> Result<String, String> {
        let row = &self.rows[i];
        let id = self.id(i);
        let d = self.work.join(&id);
        let perr = |e: String| format!("perseids: {e}");
        // Wait for a slot under PERSEIDS_MAX across every perseids run on this account.
        // ponytail: check-then-create, so runs that start together can overshoot by a few; fine for a cost cap.
        while boxes().map_or(0, |b| b.len()) >= self.max {
            secs(15);
        }
        self.set(i, "booting");
        lock(&self.slots)[i].start = Some(now());
        let image = fs::read_to_string(self.dir.join(".perseids/image")).unwrap_or_default().trim_end_matches('\n').to_string();
        let mut body = json!({ "type": self.ty, "noEnv": true, "ttlSeconds": self.ttl });
        if !image.is_empty() {
            body["from"] = J::String(image);
        }
        let name = format!("{}:{}{}", self.name, slug(&row.label, 40, "._-"), self.suffix);
        let b = create(&body.to_string(), &name, bx).map_err(perr)?;
        let s = ssh_to(&b, &d).map_err(perr)?;
        self.set(i, "running");
        let out = sh(s.scp(&[self.work.join("src.bundle"), self.work.join("runner.rb")], "/home/user/").stdout(Stdio::null()).stderr(Stdio::piped()))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("scp failed").to_string());
        }
        let script = if self.cmd == "exec" {
            format!("{}\nbash -lc {}", self.prep, q(&self.args[0]))
        } else {
            format!(
                "{}\ncommand -v ruby >/dev/null || {{ sudo -n apt-get update -qq && sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ruby; }} >/dev/null\nruby ~/runner.rb run {} {} {}",
                self.prep,
                q(&self.wf),
                q(&row.job),
                row.index
            )
        };
        let logf = self.logdir.join(format!("{id}.log"));
        let log = fs::File::create(&logf).map_err(|e| e.to_string())?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let st = sh(s.ssh(&format!("bash -lc {}", q(&script))).stdout(log).stderr(err))?.status;
        lock(&self.slots)[i].end = Some(now());
        destroy(&b);
        *bx = None;
        let text = String::from_utf8_lossy(&fs::read(&logf).unwrap_or_default()).into_owned();
        if let Some(r) = text.lines().filter(|l| l.starts_with("PERSEIDS_RESULT ")).last() {
            return Ok(r["PERSEIDS_RESULT ".len()..].replace('\t', " "));
        }
        Ok(match exit_code(st) {
            0 => "pass".into(),
            255 => "error the SSH link dropped (rerun)".into(),
            rc if self.cmd == "exec" => format!("fail exit {rc}"),
            rc => format!("error the box run stopped (exit {rc})"),
        })
    }

    /// The live board: redrawn in a terminal, one line per change otherwise (agents read that).
    fn board(&self, tty: bool, drawn: &mut usize) {
        let t = now();
        let p = price(&self.ty);
        let mut out = String::new();
        let mut slots = lock(&self.slots);
        for (r, s) in self.rows.iter().zip(slots.iter_mut()) {
            let st = s.state.clone().unwrap_or_else(|| "waiting".into());
            let secs = s.start.map_or(0, |a| s.end.unwrap_or(t).saturating_sub(a));
            if tty {
                out += &format!("  {:<12} {:<44} {:>3}m{:02}s ${:.3}\x1b[K\n", first_word(&st), chars(&r.label, 44), secs / 60, secs % 60, secs as f64 / 3600.0 * p);
            } else if s.seen != st {
                say(format!("{}: {st}", r.label));
                s.seen = st;
            }
        }
        if tty {
            if *drawn > 0 {
                eprint!("\x1b[{}A", *drawn);
            }
            eprint!("{out}");
            *drawn = self.rows.len();
        }
    }

    fn go(&self) -> i32 {
        let tty = std::io::stderr().is_terminal();
        let mut drawn = 0;
        let mut waves: Vec<usize> = self.rows.iter().map(|r| r.wave).collect();
        waves.sort_unstable();
        waves.dedup();
        std::thread::scope(|sc| {
            let mut live = vec![];
            for w in waves {
                for (i, r) in self.rows.iter().enumerate().filter(|(_, r)| r.wave == w) {
                    fs::create_dir_all(self.work.join(self.id(i))).ok();
                    if r.state != "run" {
                        self.set(i, &format!("{} {}", r.state, if r.note.is_empty() { "-" } else { &r.note }));
                        continue;
                    }
                    let mut blocked = String::new();
                    for n in r.needs.split(',').filter(|n| !n.is_empty()) {
                        let deps: Vec<usize> = (0..self.rows.len()).filter(|k| self.rows[*k].job == n).collect();
                        if deps.is_empty() {
                            blocked += &format!(" {n}");
                        }
                        for k in deps {
                            let st = self.state(k);
                            if !(st.starts_with("pass") || st.starts_with("incomplete")) {
                                blocked += &format!(" {n}");
                            }
                        }
                    }
                    if !blocked.is_empty() {
                        self.set(i, &format!("skip needs{blocked}"));
                        continue;
                    }
                    self.set(i, "queued");
                    loop {
                        live.retain(|h: &std::thread::ScopedJoinHandle<()>| !h.is_finished());
                        if live.len() < self.max {
                            break;
                        }
                        self.board(tty, &mut drawn);
                        secs(2);
                    }
                    live.push(sc.spawn(move || self.worker(i)));
                }
                // ponytail: a wave waits for all of the one before it; start each job when its own needs finish if waves get long.
                while live.iter().any(|h| !h.is_finished()) {
                    self.board(tty, &mut drawn);
                    secs(2);
                }
            }
        });
        self.board(tty, &mut drawn);
        self.summary()
    }

    fn summary(&self) -> i32 {
        let mut rc = 0;
        let mut total = 0;
        let tail: usize = env("PERSEIDS_TAIL").and_then(|t| t.parse().ok()).unwrap_or(40);
        let slots = lock(&self.slots).clone();
        eprintln!("{}", self.head);
        for (i, (r, s)) in self.rows.iter().zip(&slots).enumerate() {
            let st = s.state.clone().unwrap_or_default();
            let start = s.start.unwrap_or(0);
            total += s.end.unwrap_or(start).saturating_sub(start);
            let word = first_word(&st);
            eprintln!("  {:<12} {:<44} {}", word, chars(&r.label, 44), st.split_once(' ').map_or("", |x| x.1));
            match word {
                "fail" => rc = 1,
                "error" if rc != 1 => rc = 99,
                "incomplete" | "unsupported" if rc == 0 => rc = 2,
                _ => {}
            }
            let log = self.logdir.join(format!("{}.log", self.id(i)));
            if (word == "fail" || word == "error") && log.exists() {
                eprintln!("  ↳ {}, last {tail} lines:", log.display());
                let text = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
                let lines: Vec<&str> = text.lines().collect();
                for l in &lines[lines.len().saturating_sub(tail)..] {
                    eprintln!("    | {l}");
                }
            }
        }
        let p = price(&self.ty);
        eprintln!("--- exit {rc} · {} box-min, about ${:.2} · logs {}", total / 60, total as f64 / 3600.0 * p, self.logdir.display());
        rc
    }
}

fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
}

/// owner/repo from a remote URL: sed -E 's#^.*[:/]([^/]+/[^/]+)$#\1#; s#\.git$##'
fn origin(url: &str) -> String {
    let cut = url.char_indices().rev().filter(|(_, c)| *c == ':' || *c == '/').map(|(i, _)| &url[i + 1..]).find(|r| {
        r.matches('/').count() == 1 && !r.starts_with('/') && !r.ends_with('/')
    });
    let r = cut.unwrap_or(url);
    r.strip_suffix(".git").unwrap_or(r).to_string()
}

/// The `key` command's prompt: no echo on a terminal.
fn read_key() -> String {
    let mut k = String::new();
    if std::io::stdin().is_terminal() {
        eprint!("Boat API key (boat.dev, Settings, API keys): ");
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            let ok = libc::tcgetattr(0, &mut t) == 0;
            let old = t;
            t.c_lflag &= !libc::ECHO;
            if ok {
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
            std::io::stdin().lock().read_line(&mut k).ok();
            if ok {
                libc::tcsetattr(0, libc::TCSANOW, &old);
            }
        }
        eprintln!();
    } else {
        std::io::stdin().lock().read_line(&mut k).ok();
    }
    k.trim().to_string()
}

fn usage(code: i32) -> ! {
    print!("{USAGE}");
    finish(code)
}

fn main() {
    let _ = ctrlc::set_handler(|| finish(130));
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = if argv.is_empty() { "help".to_string() } else { argv.remove(0) };
    let (mut dir, mut wf, mut keep, mut dirty, mut all, mut args) = (".".to_string(), String::new(), false, false, false, vec![]);
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-C" => dir = it.next().unwrap_or_else(|| die("-C needs a folder")),
            "-w" | "--workflow" => wf = it.next().unwrap_or_else(|| die("-w needs a workflow")),
            "--keep-going" => keep = true,
            "--dirty" => dirty = true,
            "--all" => all = true,
            "-h" | "--help" => usage(0),
            f if f.starts_with('-') && f.len() > 1 => {
                say(format!("unknown flag {f}"));
                usage(64)
            }
            _ => args.push(a),
        }
    }
    if std::env::set_current_dir(&dir).is_err() {
        die(format!("no folder {dir}"));
    }
    let dir = std::env::current_dir().unwrap_or_default();

    // Hidden: `perseids plan <workflow> [job...]` prints what `ruby runner.rb plan` prints (the parity test uses it).
    if cmd == "plan" {
        let Some(path) = args.first() else { die("plan needs a workflow") };
        let wf = fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| plan::load(&t)).unwrap_or_else(|e| {
            say(e);
            finish(1)
        });
        match plan::plan(&wf, &args[1..]) {
            Ok(rows) => rows.iter().for_each(|r| println!("{}", r.tsv())),
            Err(plan::Unsupported(e)) => {
                say(e);
                finish(2)
            }
        }
        finish(0)
    }

    let boat = env("BOAT_API").unwrap_or_else(|| "https://boat.dev/api/v1".into());
    let key_file = env("PERSEIDS_KEY_FILE").unwrap_or_else(|| format!("{}/.config/perseids/boat.env", home()));
    let ty = env("PERSEIDS_TYPE").unwrap_or_else(|| "large".into());
    let max: usize = env("PERSEIDS_MAX").map_or(Ok(10), |m| m.parse()).unwrap_or_else(|_| die("PERSEIDS_MAX must be a number"));
    let ttl: u64 = env("PERSEIDS_TTL").map_or(Ok(7200), |m| m.parse()).unwrap_or_else(|_| die("PERSEIDS_TTL must be a number"));
    let host = hostname();
    let pid = std::process::id();
    let work = std::env::temp_dir().join(format!("perseids-{pid}-{}", now()));
    fs::create_dir_all(&work).unwrap_or_else(|e| die(format!("no temp folder: {e}")));
    let _ = WORK.set(work.clone());

    let key = || {
        let k = env("BOAT_API_KEY").or_else(|| env_file(&key_file, "BOAT_API_KEY").map(|k| k.replace(['"', '\'', ' '], ""))).unwrap_or_default();
        if k.is_empty() {
            die("no Boat key: run perseids key (or set BOAT_API_KEY)");
        }
        let _ = API.set((boat.clone(), k));
    };

    match cmd.as_str() {
        "help" | "-h" | "--help" => usage(0),
        "key" => {
            // the one place your Boat key lives: ~/.config/perseids/boat.env, mode 600
            let k = read_key();
            if !k.starts_with("boat_") {
                die("that isn't a Boat key: they start with boat_");
            }
            let _ = API.set((boat.clone(), k.clone())); // checked against Boat itself
            let ok = api("GET", "/sandboxes?limit=1", None, None).map(|r| j(&r).get("sandboxes").is_some());
            if ok != Ok(true) {
                die("Boat didn't accept that key");
            }
            write_secret(&key_file, &format!("BOAT_API_KEY={k}\n")).unwrap_or_else(|e| die(e));
            say(format!("key saved to {key_file}; perseids run will use it"));
            finish(0)
        }
        "ls" => {
            key();
            for (id, st, name) in boxes().unwrap_or_else(|e| die(e)) {
                println!("{id} {st} {name}");
            }
            finish(0)
        }
        "reap" => {
            key();
            reap(all).unwrap_or_else(|e| die(e));
            finish(0)
        }
        "run" | "exec" | "image" => key(),
        _ => {
            say(format!("unknown command {cmd}"));
            usage(64)
        }
    }
    if cmd == "image" {
        image_build(&args, &dir, &work, &ty, ttl, &host);
    }

    // --- the commit to test ---
    let top = git(&["rev-parse", "--show-toplevel"]).unwrap_or_else(|| die(format!("{} isn't a git repo", dir.display())));
    std::env::set_current_dir(&top).unwrap_or_else(|e| die(e));
    let dir = PathBuf::from(&top);
    let mut sha = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| die("this repo has no commit yet"));
    let mut tag = sha[..8.min(sha.len())].to_string();
    if !Command::new("git").args(["diff", "--quiet", "HEAD", "--"]).status().is_ok_and(|s| s.success()) {
        if !dirty {
            die("uncommitted changes to tracked files: commit them (perseids tests a commit), or pass --dirty");
        }
        sha = git(&["stash", "create"]).filter(|s| !s.is_empty()).unwrap_or_else(|| die("git stash create failed")); // a commit of the working tree; it touches nothing here
        tag += "+dirty";
    }
    let repo = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let org = git(&["remote", "get-url", "origin"]).map(|u| origin(&u)).unwrap_or_default();
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let state_home = env("XDG_STATE_HOME").unwrap_or_else(|| format!("{}/.local/state", home()));
    let logdir = PathBuf::from(format!("{state_home}/perseids/logs/{repo}-{tag}-{}", strftime("%Y%m%d-%H%M%S", false)));
    fs::create_dir_all(&logdir).unwrap_or_else(|e| die(e));
    let rf = format!("refs/perseids/run-{pid}");
    git(&["update-ref", &rf, &sha]).unwrap_or_else(|| die("git update-ref failed"));
    let bundled = Command::new("git").args(["bundle", "create", "-q"]).arg(work.join("src.bundle")).arg(&rf).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    git(&["update-ref", "-d", &rf]);
    if !bundled {
        die("git bundle failed");
    }
    fs::write(work.join("runner.rb"), RUNNER).unwrap_or_else(|e| die(e));
    let event = env("PERSEIDS_EVENT").unwrap_or_else(|| "push".into());
    let prep = format!(
        "set -eo pipefail\ngit init -q ~/w && cd ~/w && git fetch -q ~/src.bundle {rf} && git checkout -q --detach {sha}\nexport PERSEIDS_SHA={sha} PERSEIDS_REF_NAME={} PERSEIDS_REPOSITORY={} PERSEIDS_EVENT={}{}",
        q(&branch),
        q(&org),
        q(&event),
        if keep { " PERSEIDS_KEEP_GOING=1" } else { "" }
    );

    // --- the work list: one row per box ---
    let rows = if cmd == "exec" {
        if args.first().is_none_or(|a| a.is_empty()) {
            die("exec needs a command");
        }
        vec![plan::Row { wave: 0, job: "exec".into(), index: 0, state: "run".into(), needs: String::new(), label: "exec".into(), note: String::new() }]
    } else {
        if wf.is_empty() {
            wf = ["ci.yml", "ci.yaml"].iter().map(|f| format!(".github/workflows/{f}")).find(|f| Path::new(f).is_file()).unwrap_or_default();
            if wf.is_empty() {
                let mut all: Vec<String> = fs::read_dir(".github/workflows")
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.rsplit_once('.').is_some_and(|(_, x)| x.starts_with('y') && x.ends_with("ml")))
                    .map(|n| format!(".github/workflows/{n}"))
                    .collect();
                all.sort();
                if all.len() != 1 {
                    die(format!("pick a workflow with -w (there are {} in .github/workflows)", all.len()));
                }
                wf = all.remove(0);
            }
        }
        if !Path::new(&wf).is_file() {
            wf = format!(".github/workflows/{wf}");
        }
        if !Path::new(&wf).is_file() {
            die(format!("no workflow {wf}"));
        }
        let planned = fs::read_to_string(&wf).map_err(|e| e.to_string()).and_then(|t| plan::load(&t)).and_then(|w| plan::plan(&w, &args).map_err(|plan::Unsupported(e)| e));
        planned.unwrap_or_else(|e| {
            say(e);
            die(format!("couldn't plan {wf}"))
        })
    };
    // what runner.rb's TSV would have carried
    let rows: Vec<plan::Row> = rows
        .into_iter()
        .map(|mut r| {
            r.label = r.label.replace(['\t', '\n'], " ");
            r.note = r.note.replace(['\t', '\n'], " ");
            r
        })
        .collect();

    let run = Run {
        slots: Mutex::new(vec![Slot::default(); rows.len()]),
        rows,
        cmd: cmd.clone(),
        args,
        work,
        logdir,
        dir,
        wf: wf.clone(),
        prep,
        head: format!("--- perseids: {repo}@{tag} ({sha}) · {wf}"),
        name: format!("perseids:{repo}@{tag}"),
        suffix: format!("#{host}:{pid}"),
        ty: ty.clone(),
        ttl,
        max,
    };
    say(format!("{repo}@{tag} on {ty} boxes, at most {max} at once · logs {}", run.logdir.display()));
    let _ = reap(false);
    let rc = run.go();
    finish(rc)
}

/// image build: a box runs images/base.sh and the repo's .perseids/setup.sh, then is saved as a named snapshot.
fn image_build(args: &[String], dir: &Path, work: &Path, ty: &str, ttl: u64, host: &str) -> ! {
    if args.first().map(String::as_str) != Some("build") {
        usage(64);
    }
    let top = git(&["rev-parse", "--show-toplevel"]).map(PathBuf::from).unwrap_or(dir.to_path_buf());
    let repo = top.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = format!("perseids-{}-{}", slug(&repo, 30, "-"), strftime("%Y%m%d-%H%M", true));
    say(format!("1/3 a fresh {ty} box runs the setup"));
    let mut slot = None;
    let body = json!({ "type": ty, "noEnv": true, "ttlSeconds": ttl }).to_string();
    let b = create(&body, &format!("perseids:image:{name}#{host}:{}", std::process::id()), &mut slot).unwrap_or_else(|e| die(e));
    let d = work.join("img");
    fs::create_dir_all(&d).unwrap_or_else(|e| die(e));
    let s = ssh_to(&b, &d).unwrap_or_else(|e| die(e));
    let base = work.join("perseids-base.sh");
    fs::write(&base, BASE_SH).unwrap_or_else(|e| die(e));
    let mut files = vec![base];
    let mut steps = "sudo -n bash /home/user/perseids-base.sh".to_string();
    let setup = dir.join(".perseids/setup.sh");
    if setup.is_file() {
        let to = work.join("perseids-setup.sh");
        fs::copy(&setup, &to).unwrap_or_else(|e| die(e));
        files.push(to);
        steps += " && sudo -n bash /home/user/perseids-setup.sh";
    }
    if !sh(&mut s.scp(&files, "/home/user/")).is_ok_and(|o| o.status.success()) {
        die("couldn't copy the setup to the box");
    }
    if !sh(&mut s.ssh(&format!("{steps} && rm -f /home/user/perseids-*.sh"))).is_ok_and(|o| o.status.success()) {
        die("the setup failed");
    }
    say(format!("2/3 saving the image {name}"));
    let _ = api("POST", &format!("/sandboxes/{b}/stop"), None, None);
    wait_state(&b, &["archived"]).unwrap_or_else(|e| die(e));
    let _ = api("POST", "/named-snapshots", Some(&json!({ "sandboxId": b, "name": name }).to_string()), None);
    let mut st = "saving".to_string();
    for _ in 0..90 {
        let r = j(&api("GET", "/named-snapshots", None, None).unwrap_or_default());
        st = r["snapshots"].as_array().into_iter().flatten().filter(|x| x["name"] == J::String(name.clone())).map(|x| js(&x["status"])).collect::<Vec<_>>().join("\n");
        if st != "saving" {
            break;
        }
        secs(10);
    }
    if st != "ready" {
        die(format!("the image {name} is {st}"));
    }
    let file = dir.join(".perseids/image");
    let old = fs::read_to_string(&file).unwrap_or_default().trim_end_matches('\n').to_string();
    fs::create_dir_all(dir.join(".perseids")).unwrap_or_else(|e| die(e));
    fs::write(&file, format!("{name}\n")).unwrap_or_else(|e| die(e));
    // The old image goes once nothing new starts from it (running boxes already have their disks).
    if !old.is_empty() && old != name && api("DELETE", &format!("/named-snapshots/{old}"), None, None).is_err() {
        say(format!("the old image {old} wasn't deleted"));
    }
    say(format!("3/3 image {name} saved in .perseids/image (commit it); the build box is deleted"));
    finish(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_like_sed() {
        assert_eq!(origin("git@github.com:kkeeland/perseids.git"), "kkeeland/perseids");
        assert_eq!(origin("https://github.com/kkeeland/perseids"), "kkeeland/perseids");
        assert_eq!(origin("/local/path"), "local/path");
        assert_eq!(origin("weird"), "weird");
    }

    #[test]
    fn quoting_and_slugs() {
        assert_eq!(q("it's"), r"'it'\''s'");
        assert_eq!(slug("services (16, a/b)", 40, "._-"), "services--16--a-b-");
        assert_eq!(first_word("skip needs a b"), "skip");
    }
}
