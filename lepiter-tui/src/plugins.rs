//! external snippet renderer integration via json ipc.
//!
//! plugins are configured with `LEPITER_PLUGIN_CONFIG` and receive snippet
//! render requests over stdin/stdout as json lines.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use lepiter_core::plugin::{PluginRequest, PluginResponse};

use crate::util::{LruCache, cache_limit_from_env_allow_zero};

#[derive(Debug, Deserialize)]
struct PluginConfig {
    #[serde(default)]
    plugins: Vec<PluginSpec>,
}

#[derive(Debug, Deserialize)]
struct PluginSpec {
    name: String,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    types: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum PluginRender {
    Lines(Vec<String>),
    Error(String),
}

/// A failed plugin request; the retry loop respawns on either variant.
#[derive(Debug)]
enum RequestError {
    /// Plugin did not respond within the deadline, with any stderr it wrote.
    Timeout(Duration, String),
    /// Plugin process died, produced bad output, or hit an I/O error.
    Failed(anyhow::Error),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::Timeout(d, tail) => {
                let msg = format!("timed out after {}ms", d.as_millis());
                write!(f, "{}", with_stderr(msg, tail))
            }
            RequestError::Failed(e) => write!(f, "{e}"),
        }
    }
}

/// Appends a plugin's stderr tail to a failure message, if it wrote any.
fn with_stderr(msg: String, tail: &str) -> String {
    if tail.is_empty() {
        msg
    } else {
        format!("{msg} (stderr: {tail})")
    }
}

/// Bytes of a plugin's stderr kept for error messages.
const DEFAULT_STDERR_BYTES: usize = 2048;

/// How long a failure waits for the drain thread to flush the stderr of a
/// plugin that is expected to be gone.
const STDERR_FLUSH_WAIT: Duration = Duration::from_millis(200);

#[derive(Default)]
struct TailState {
    buf: VecDeque<u8>,
    finished: bool,
}

/// Bounded capture of a plugin's stderr: keeps the tail, drops the head.
struct StderrTail {
    cap: usize,
    state: Mutex<TailState>,
    finished: Condvar,
}

impl StderrTail {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            state: Mutex::new(TailState::default()),
            finished: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TailState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, chunk: &[u8]) {
        if self.cap == 0 {
            return;
        }
        let mut state = self.lock();
        state.buf.extend(chunk);
        let overflow = state.buf.len().saturating_sub(self.cap);
        state.buf.drain(..overflow);
    }

    fn finish(&self) {
        self.lock().finished = true;
        self.finished.notify_all();
    }

    /// Captured tail as one sanitized line, waiting up to `flush` for the
    /// drain thread to finish first.
    fn snapshot(&self, flush: Duration) -> String {
        if self.cap == 0 {
            return String::new();
        }
        let mut state = self.lock();
        if !state.finished && !flush.is_zero() {
            let (guard, _) = self
                .finished
                .wait_timeout_while(state, flush, |s| !s.finished)
                .unwrap_or_else(|e| e.into_inner());
            state = guard;
        }
        let bytes = state.buf.iter().copied().collect::<Vec<u8>>();
        drop(state);
        // The tail can start mid-character; skip to the first boundary.
        let start = bytes
            .iter()
            .position(|b| b & 0xC0 != 0x80)
            .unwrap_or(bytes.len());
        one_line(&String::from_utf8_lossy(&bytes[start..]))
    }
}

/// Flattens captured output to a single control-byte-free line.
fn one_line(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut gap = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            gap = !out.is_empty();
        } else if !ch.is_control() {
            if gap {
                out.push(' ');
                gap = false;
            }
            out.push(ch);
        }
    }
    out
}

/// Records a failure message unless an identical attempt already did.
fn push_distinct(msgs: &mut Vec<String>, msg: String) {
    if !msgs.contains(&msg) {
        msgs.push(msg);
    }
}

fn drain_stderr(mut stderr: ChildStderr, tail: Arc<StderrTail>) {
    let mut chunk = [0u8; 1024];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => tail.push(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    tail.finish();
}

struct PluginProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    /// Receives lines read by a background reader thread.
    rx: mpsc::Receiver<io::Result<String>>,
    stderr: Arc<StderrTail>,
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PluginProcess {
    fn spawn(binary: &str, args: &[String], stderr_cap: usize) -> Result<Self> {
        let mut cmd = Command::new(binary);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if stderr_cap == 0 {
                Stdio::null()
            } else {
                Stdio::piped()
            });
        let mut child = cmd.spawn().with_context(|| format!("spawn {binary}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing plugin stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing plugin stdout"))?;

        let stderr = Arc::new(StderrTail::new(stderr_cap));
        if let Some(handle) = child.stderr.take() {
            let tail = Arc::clone(&stderr);
            thread::spawn(move || drain_stderr(handle, tail));
        }

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = tx.send(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "plugin stdout closed unexpectedly (process likely crashed)",
                        )));
                        break;
                    }
                    Ok(_) => {
                        if tx.send(Ok(line)).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            rx,
            stderr,
        })
    }

    fn request_with_timeout(
        &mut self,
        req: &PluginRequest,
        timeout: Duration,
    ) -> Result<PluginResponse, RequestError> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|e| RequestError::Failed(e.into()))?
        {
            return Err(self.failed(format!("plugin exited: {status}"), STDERR_FLUSH_WAIT));
        }

        let mut payload = serde_json::to_string(req).map_err(|e| RequestError::Failed(e.into()))?;
        payload.push('\n');
        let written = self
            .stdin
            .write_all(payload.as_bytes())
            .and_then(|()| self.stdin.flush());
        if let Err(e) = written {
            return Err(self.failed(e.to_string(), STDERR_FLUSH_WAIT));
        }

        match self.rx.recv_timeout(timeout) {
            Ok(Ok(line)) => match serde_json::from_str::<PluginResponse>(&line) {
                Ok(resp) => Ok(resp),
                Err(e) => Err(self.failed(e.to_string(), Duration::ZERO)),
            },
            Ok(Err(io_err)) => {
                Err(self.failed(format!("plugin stdout error: {io_err}"), STDERR_FLUSH_WAIT))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(RequestError::Timeout(
                timeout,
                self.stderr.snapshot(Duration::ZERO),
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.failed(
                "plugin reader thread terminated unexpectedly".to_string(),
                STDERR_FLUSH_WAIT,
            )),
        }
    }

    /// Wraps a failure with the plugin's stderr tail; `flush` is how long to
    /// wait for the drain thread, and is zero unless the process is gone.
    fn failed(&self, msg: String, flush: Duration) -> RequestError {
        let tail = self.stderr.snapshot(flush);
        RequestError::Failed(anyhow::anyhow!("{}", with_stderr(msg, &tail)))
    }
}

struct PluginHandle {
    name: String,
    /// Resolved binary path, kept for respawning after a timeout.
    binary: String,
    /// Arguments passed to the binary, kept for respawning.
    args: Vec<String>,
    process: PluginProcess,
}

pub struct PluginManager {
    processes: Vec<PluginHandle>,
    by_type: HashMap<String, usize>,
    cache: LruCache<(String, u64), PluginRender>,
    timeout: Duration,
    retries: usize,
    stderr_cap: usize,
    notes: Vec<String>,
}

impl PluginManager {
    fn env_defaults() -> (
        LruCache<(String, u64), PluginRender>,
        Duration,
        usize,
        usize,
    ) {
        let cache = LruCache::new(cache_limit_from_env_allow_zero("LEPITER_PLUGIN_CACHE", 128));
        let timeout = Duration::from_millis(
            std::env::var("LEPITER_PLUGIN_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(250),
        );
        let retries = std::env::var("LEPITER_PLUGIN_RETRIES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1);
        let stderr_cap = std::env::var("LEPITER_PLUGIN_STDERR_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_STDERR_BYTES);
        (cache, timeout, retries, stderr_cap)
    }

    pub fn empty() -> Self {
        let (cache, timeout, retries, stderr_cap) = Self::env_defaults();
        Self {
            processes: Vec::new(),
            by_type: HashMap::new(),
            cache,
            timeout,
            retries,
            stderr_cap,
            notes: Vec::new(),
        }
    }

    pub fn from_env() -> Self {
        let Ok(path) = std::env::var("LEPITER_PLUGIN_CONFIG") else {
            return Self::empty();
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                let mut mgr = Self::empty();
                mgr.notes.push(format!("plugin config read failed: {err}"));
                return mgr;
            }
        };
        let config: PluginConfig = match serde_json::from_slice(&bytes) {
            Ok(config) => config,
            Err(err) => {
                let mut mgr = Self::empty();
                mgr.notes.push(format!("plugin config parse failed: {err}"));
                return mgr;
            }
        };

        let (cache, timeout, retries, stderr_cap) = Self::env_defaults();
        let mut processes = Vec::new();
        let mut by_type = HashMap::new();
        let mut notes = Vec::new();
        for plugin in config.plugins {
            let (process, binary) = match spawn_plugin_process(&plugin, stderr_cap) {
                Ok(result) => result,
                Err(err) => {
                    notes.push(format!("plugin {} failed to start: {err}", plugin.name));
                    continue;
                }
            };
            let idx = processes.len();
            processes.push(PluginHandle {
                name: plugin.name.clone(),
                binary,
                args: plugin.args.clone(),
                process,
            });
            for typ in plugin.types {
                if by_type.contains_key(&typ) {
                    notes.push(format!("plugin type already registered: {typ}"));
                    continue;
                }
                by_type.insert(typ, idx);
            }
        }

        Self {
            processes,
            by_type,
            cache,
            timeout,
            retries,
            stderr_cap,
            notes,
        }
    }

    pub fn apply_status(&mut self, status: &mut String) {
        if !self.notes.is_empty() && status.is_empty() {
            *status = self.notes.join(" | ");
        }
    }

    pub fn render(&mut self, typ: &str, raw: &Value) -> Option<PluginRender> {
        let idx = *self.by_type.get(typ)?;
        if idx >= self.processes.len() {
            return None;
        }

        let key = (typ.to_string(), hash_value(raw));
        if let Some(hit) = self.cache.get(&key).cloned() {
            return Some(hit);
        }

        let req = PluginRequest {
            typ: typ.to_string(),
            snippet: raw.clone(),
        };
        let mut errs: Vec<String> = Vec::new();
        for _ in 0..=self.retries {
            let result = self.processes[idx]
                .process
                .request_with_timeout(&req, self.timeout);
            match result {
                Ok(resp) => {
                    let rendered = if resp.ok {
                        PluginRender::Lines(resp.lines)
                    } else {
                        let msg = resp
                            .error
                            .unwrap_or_else(|| "plugin returned error".to_string());
                        PluginRender::Error(format!("{}: {}", self.processes[idx].name, msg))
                    };
                    self.cache.insert(key, rendered.clone());
                    return Some(rendered);
                }
                Err(e) => {
                    push_distinct(&mut errs, format!("{e}"));
                    // Kill the hung/dead plugin and try to respawn for the next
                    // retry attempt. Respawning is essential after a timeout
                    // because the old channel may contain a stale response.
                    let binary = self.processes[idx].binary.clone();
                    let args = self.processes[idx].args.clone();
                    match PluginProcess::spawn(&binary, &args, self.stderr_cap) {
                        Ok(new_process) => {
                            self.processes[idx].process = new_process;
                        }
                        Err(spawn_err) => {
                            push_distinct(&mut errs, format!("respawn failed: {spawn_err}"));
                            break;
                        }
                    }
                }
            }
        }
        let err = if errs.is_empty() {
            "plugin failed".to_string()
        } else {
            errs.join("; ")
        };
        let rendered = PluginRender::Error(format!("{}: {err}", self.processes[idx].name));
        // Don't cache transport-level errors (timeout/crash) — they are
        // transient and the plugin may recover after respawn.  Plugin-returned
        // errors (resp.ok == false) are still cached above because they are
        // deterministic for the same input.
        Some(rendered)
    }

    pub fn status_line(&self) -> String {
        let mut out = format!(
            "plugins: {} | cache: {}/{} | timeout: {}ms | retries: {}",
            self.processes.len(),
            self.cache.len(),
            self.cache.capacity(),
            self.timeout.as_millis(),
            self.retries
        );
        if !self.by_type.is_empty() {
            out.push_str(" | types: ");
            let mut types = self.by_type.keys().cloned().collect::<Vec<_>>();
            types.sort();
            out.push_str(&types.join(", "));
        }
        if !self.notes.is_empty() {
            out.push_str(" | ");
            out.push_str(&self.notes.join(" | "));
        }
        out
    }
}

/// Tries candidate binary names for a plugin spec. Returns the spawned
/// process and the resolved binary name (needed for respawning later).
fn spawn_plugin_process(plugin: &PluginSpec, stderr_cap: usize) -> Result<(PluginProcess, String)> {
    let mut candidates = Vec::new();
    if let Some(binary) = plugin.binary.as_ref() {
        candidates.push(binary.clone());
    } else {
        candidates.push(format!("lepiter-plugin-{}", plugin.name));
        candidates.push(format!("lepiter-{}", plugin.name));
        candidates.push(plugin.name.clone());
    }

    let mut last_err = None;
    for candidate in candidates {
        match PluginProcess::spawn(&candidate, &plugin.args, stderr_cap) {
            Ok(process) => return Ok((process, candidate)),
            Err(err) => {
                last_err = Some((candidate, err));
                continue;
            }
        }
    }

    if let Some((candidate, err)) = last_err {
        bail!("no plugin binary found (last tried `{candidate}`): {err}");
    }

    bail!("no plugin binary candidates")
}

fn hash_value(value: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    match serde_json::to_string(value) {
        Ok(json) => json.hash(&mut hasher),
        Err(_) => "<invalid>".hash(&mut hasher),
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_request() -> PluginRequest {
        PluginRequest {
            typ: "test".to_string(),
            snippet: serde_json::json!({}),
        }
    }

    fn hung_plugin() -> PluginProcess {
        let script = "read line; sleep 60";
        PluginProcess::spawn(
            "bash",
            &["-c".to_string(), script.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap()
    }

    #[test]
    fn working_plugin_responds_within_timeout() {
        let script = r#"read line; echo '{"ok":true,"lines":["hello"],"error":null}'"#;
        let mut proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), script.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();
        let req = dummy_request();
        let result = proc.request_with_timeout(&req, Duration::from_secs(5));
        match result {
            Ok(resp) => {
                assert!(resp.ok);
                assert_eq!(resp.lines, vec!["hello"]);
            }
            Err(e) => panic!("expected Ok, got: {e}"),
        }
    }

    #[test]
    fn request_times_out_on_unresponsive_plugin() {
        // `sleep 60` outlives the 100ms timeout below and never writes stdout.
        // (`sleep infinity` is GNU-only — BSD `sleep` on macOS rejects it.)
        let mut proc =
            PluginProcess::spawn("sleep", &["60".to_string()], DEFAULT_STDERR_BYTES).unwrap();
        let req = dummy_request();
        let start = std::time::Instant::now();
        let result = proc.request_with_timeout(&req, Duration::from_millis(100));
        let elapsed = start.elapsed();
        match result {
            Err(ref err @ RequestError::Timeout(..)) => {
                assert_eq!(err.to_string(), "timed out after 100ms");
                // Should have taken roughly the timeout duration, not much more.
                assert!(elapsed < Duration::from_millis(500));
            }
            other => panic!("expected Timeout, got: {other:?}"),
        }
    }

    #[test]
    fn request_detects_mid_request_crash() {
        // Plugin reads the request then exits without responding.
        let script = "read line; exit 1";
        let mut proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), script.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();
        let req = dummy_request();
        let result = proc.request_with_timeout(&req, Duration::from_secs(5));
        assert!(
            matches!(result, Err(RequestError::Failed(_))),
            "expected Failed, got: {result:?}"
        );
    }

    #[test]
    fn request_detects_already_exited_plugin() {
        let mut proc = PluginProcess::spawn("true", &[], DEFAULT_STDERR_BYTES).unwrap();
        // Give it a moment to exit.
        std::thread::sleep(Duration::from_millis(50));
        let req = dummy_request();
        let result = proc.request_with_timeout(&req, Duration::from_secs(1));
        assert!(
            matches!(result, Err(RequestError::Failed(_))),
            "expected Failed, got: {result:?}"
        );
    }

    #[test]
    fn render_with_working_plugin() {
        let script =
            r#"while read line; do echo '{"ok":true,"lines":["rendered"],"error":null}'; done"#;
        let proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), script.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();

        let mut mgr = PluginManager::empty();
        mgr.processes.push(PluginHandle {
            name: "test".to_string(),
            binary: "bash".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            process: proc,
        });
        mgr.by_type.insert("testType".to_string(), 0);

        let result = mgr.render("testType", &serde_json::json!({"data": "test"}));
        match result {
            Some(PluginRender::Lines(lines)) => assert_eq!(lines, vec!["rendered"]),
            other => panic!("expected Lines, got: {other:?}"),
        }
    }

    #[test]
    fn render_respawns_hung_plugin() {
        let proc = hung_plugin();

        // The handle's binary/args point to a *working* plugin, so the
        // respawn produces a process that actually responds.
        let work_script =
            r#"while read line; do echo '{"ok":true,"lines":["recovered"],"error":null}'; done"#;

        let mut mgr = PluginManager::empty();
        mgr.timeout = Duration::from_millis(100);
        mgr.retries = 1;
        mgr.processes.push(PluginHandle {
            name: "test".to_string(),
            binary: "bash".to_string(),
            args: vec!["-c".to_string(), work_script.to_string()],
            process: proc,
        });
        mgr.by_type.insert("testType".to_string(), 0);

        let result = mgr.render("testType", &serde_json::json!({"data": "test"}));
        match result {
            Some(PluginRender::Lines(lines)) => assert_eq!(lines, vec!["recovered"]),
            other => panic!("expected Lines after respawn, got: {other:?}"),
        }
    }

    #[test]
    fn render_returns_error_when_respawn_fails() {
        let proc = hung_plugin();

        let mut mgr = PluginManager::empty();
        mgr.timeout = Duration::from_millis(100);
        mgr.retries = 1;
        mgr.processes.push(PluginHandle {
            name: "test".to_string(),
            binary: "nonexistent-binary-xyz".to_string(),
            args: vec![],
            process: proc,
        });
        mgr.by_type.insert("testType".to_string(), 0);

        let result = mgr.render("testType", &serde_json::json!({}));
        match result {
            Some(PluginRender::Error(msg)) => {
                assert!(msg.contains("respawn failed"), "unexpected error: {msg}");
            }
            other => panic!("expected Error, got: {other:?}"),
        }
    }

    #[test]
    fn transient_error_not_cached() {
        // The error must NOT be cached, so the same key can succeed on a
        // later render.
        let proc = hung_plugin();

        let work_script =
            r#"while read line; do echo '{"ok":true,"lines":["recovered"],"error":null}'; done"#;

        let mut mgr = PluginManager::empty();
        mgr.timeout = Duration::from_millis(100);
        mgr.retries = 0; // no retries — fail immediately
        mgr.processes.push(PluginHandle {
            name: "test".to_string(),
            binary: "bash".to_string(),
            args: vec!["-c".to_string(), work_script.to_string()],
            process: proc,
        });
        mgr.by_type.insert("testType".to_string(), 0);

        let snippet = serde_json::json!({"same": "key"});

        // First render: the initial process hangs and times out.  With
        // retries=0 the loop runs once, respawns, then falls through to
        // the error path.
        let r1 = mgr.render("testType", &snippet);
        assert!(
            matches!(r1, Some(PluginRender::Error(ref msg)) if msg.contains("timed out")),
            "first render should fail with a timeout, got: {r1:?}"
        );

        // Second render with the SAME key: must retry the (now working)
        // respawned plugin instead of returning a cached error.
        let r2 = mgr.render("testType", &snippet);
        assert!(
            matches!(r2, Some(PluginRender::Lines(ref lines)) if lines == &["recovered"]),
            "second render should succeed after recovery, got: {r2:?}"
        );
    }

    #[test]
    fn exited_plugin_error_carries_stderr_tail() {
        let script = r#"echo "startup failed: missing token" >&2; exit 7"#;
        let mut proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), script.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let result = proc.request_with_timeout(&dummy_request(), Duration::from_secs(1));
        match result {
            Err(RequestError::Failed(ref e)) => {
                let msg = e.to_string();
                assert!(msg.contains("plugin exited"), "unexpected error: {msg}");
                assert!(
                    msg.contains("startup failed: missing token"),
                    "stderr missing from: {msg}"
                );
            }
            other => panic!("expected Failed, got: {other:?}"),
        }
    }

    #[test]
    fn mid_request_crash_error_carries_stderr_tail() {
        let script = r#"read line; echo "render failed: bad snippet" >&2; exit 1"#;
        let mut proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), script.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();
        let result = proc.request_with_timeout(&dummy_request(), Duration::from_secs(5));
        match result {
            Err(RequestError::Failed(ref e)) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("render failed: bad snippet"),
                    "stderr missing from: {msg}"
                );
            }
            other => panic!("expected Failed, got: {other:?}"),
        }
    }

    #[test]
    fn stderr_capture_keeps_the_tail_within_the_cap() {
        let tail = StderrTail::new(8);
        tail.push(b"abcdefgh");
        tail.push(b"ijkl");
        assert_eq!(tail.snapshot(Duration::ZERO), "efghijkl");
    }

    #[test]
    fn chatty_plugin_reports_its_last_stderr_not_its_first() {
        let script = r#"read line; echo HEADMARKER >&2; for i in {1..200}; do echo "filler $i" >&2; done; echo TAILMARKER >&2; exit 1"#;
        let mut proc =
            PluginProcess::spawn("bash", &["-c".to_string(), script.to_string()], 64).unwrap();
        let result = proc.request_with_timeout(&dummy_request(), Duration::from_secs(5));
        match result {
            Err(RequestError::Failed(ref e)) => {
                let msg = e.to_string();
                assert!(msg.contains("TAILMARKER"), "tail missing from: {msg}");
                assert!(!msg.contains("HEADMARKER"), "head not dropped from: {msg}");
            }
            other => panic!("expected Failed, got: {other:?}"),
        }
    }

    #[test]
    fn stderr_capture_disabled_by_a_zero_cap() {
        let script = r#"read line; echo LOUD >&2; exit 1"#;
        let mut proc =
            PluginProcess::spawn("bash", &["-c".to_string(), script.to_string()], 0).unwrap();
        let result = proc.request_with_timeout(&dummy_request(), Duration::from_secs(5));
        match result {
            Err(RequestError::Failed(ref e)) => {
                let msg = e.to_string();
                assert!(!msg.contains("LOUD"), "capture not disabled: {msg}");
                assert!(!msg.contains("stderr:"), "dangling stderr note: {msg}");
            }
            other => panic!("expected Failed, got: {other:?}"),
        }
    }

    #[test]
    fn respawned_plugin_does_not_report_the_previous_stderr() {
        let noisy = r#"read line; echo OLDSTDERR >&2; exit 1"#;
        let quiet = r#"read line; exit 1"#;
        let proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), noisy.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();

        let mut mgr = PluginManager::empty();
        mgr.timeout = Duration::from_secs(5);
        mgr.retries = 0;
        mgr.processes.push(PluginHandle {
            name: "test".to_string(),
            binary: "bash".to_string(),
            args: vec!["-c".to_string(), quiet.to_string()],
            process: proc,
        });
        mgr.by_type.insert("testType".to_string(), 0);

        match mgr.render("testType", &serde_json::json!({"n": 1})) {
            Some(PluginRender::Error(ref msg)) => {
                assert!(msg.contains("OLDSTDERR"), "stderr missing from: {msg}");
            }
            other => panic!("expected Error, got: {other:?}"),
        }

        match mgr.render("testType", &serde_json::json!({"n": 2})) {
            Some(PluginRender::Error(ref msg)) => {
                assert!(!msg.contains("OLDSTDERR"), "stale stderr in: {msg}");
                assert!(!msg.contains("stderr:"), "dangling stderr note: {msg}");
            }
            other => panic!("expected Error, got: {other:?}"),
        }
    }

    #[test]
    fn stderr_drain_thread_exits_with_the_process() {
        let script = "while true; do echo spam >&2; sleep 0.01; done";
        let proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), script.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();
        let tail = Arc::clone(&proc.stderr);
        drop(proc);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&tail) > 1 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            Arc::strong_count(&tail),
            1,
            "drain thread outlived the plugin process"
        );
    }

    #[test]
    fn timed_out_plugin_reports_the_stderr_it_already_wrote() {
        let script = r#"echo "connecting to api.example.com ..." >&2; read line; sleep 60"#;
        let mut proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), script.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let start = std::time::Instant::now();
        let result = proc.request_with_timeout(&dummy_request(), Duration::from_millis(100));
        let elapsed = start.elapsed();
        match result {
            Err(ref err @ RequestError::Timeout(..)) => {
                let msg = err.to_string();
                assert!(
                    msg.starts_with("timed out after 100ms"),
                    "unexpected: {msg}"
                );
                assert!(
                    msg.contains("connecting to api.example.com"),
                    "stderr missing from: {msg}"
                );
            }
            other => panic!("expected Timeout, got: {other:?}"),
        }
        // Alive and holding stderr open: any wait would add STDERR_FLUSH_WAIT.
        assert!(
            elapsed < Duration::from_millis(250),
            "timeout path waited on stderr: {elapsed:?}"
        );
    }

    #[test]
    fn a_crash_diagnostic_survives_a_later_bare_timeout() {
        let crashing = r#"read line; echo "REALDIAGNOSTIC: token expired" >&2; exit 1"#;
        let hanging = "read line; sleep 60";
        let proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), crashing.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();

        let mut mgr = PluginManager::empty();
        mgr.timeout = Duration::from_millis(100);
        mgr.retries = 1;
        mgr.processes.push(PluginHandle {
            name: "test".to_string(),
            binary: "bash".to_string(),
            args: vec!["-c".to_string(), hanging.to_string()],
            process: proc,
        });
        mgr.by_type.insert("testType".to_string(), 0);

        match mgr.render("testType", &serde_json::json!({})) {
            Some(PluginRender::Error(ref msg)) => {
                assert!(
                    msg.contains("REALDIAGNOSTIC: token expired"),
                    "first attempt's stderr lost from: {msg}"
                );
                assert!(msg.contains("timed out after 100ms"), "unexpected: {msg}");
            }
            other => panic!("expected Error, got: {other:?}"),
        }
    }

    #[test]
    fn identical_attempt_failures_are_reported_once() {
        let crashing = r#"read line; echo "REALDIAGNOSTIC: token expired" >&2; exit 1"#;
        let proc = PluginProcess::spawn(
            "bash",
            &["-c".to_string(), crashing.to_string()],
            DEFAULT_STDERR_BYTES,
        )
        .unwrap();

        let mut mgr = PluginManager::empty();
        mgr.timeout = Duration::from_secs(5);
        mgr.retries = 1;
        mgr.processes.push(PluginHandle {
            name: "test".to_string(),
            binary: "bash".to_string(),
            args: vec!["-c".to_string(), crashing.to_string()],
            process: proc,
        });
        mgr.by_type.insert("testType".to_string(), 0);

        match mgr.render("testType", &serde_json::json!({})) {
            Some(PluginRender::Error(ref msg)) => {
                assert_eq!(
                    msg.matches("REALDIAGNOSTIC").count(),
                    1,
                    "repeated failure listed twice: {msg}"
                );
            }
            other => panic!("expected Error, got: {other:?}"),
        }
    }

    #[test]
    fn stderr_tail_cut_mid_character_starts_at_a_boundary() {
        let tail = StderrTail::new(10);
        tail.push("日本語テスト".as_bytes());
        assert_eq!(tail.snapshot(Duration::ZERO), "テスト");

        // A cap too small for even one character leaves nothing to show.
        let sliver = StderrTail::new(2);
        sliver.push("語".as_bytes());
        assert_eq!(sliver.snapshot(Duration::ZERO), "");
    }

    #[test]
    fn multiple_requests_work_after_respawn() {
        let work_script =
            r#"while read line; do echo '{"ok":true,"lines":["ok"],"error":null}'; done"#;

        let proc = hung_plugin();

        let mut mgr = PluginManager::empty();
        mgr.timeout = Duration::from_millis(100);
        mgr.retries = 1;
        mgr.processes.push(PluginHandle {
            name: "test".to_string(),
            binary: "bash".to_string(),
            args: vec!["-c".to_string(), work_script.to_string()],
            process: proc,
        });
        mgr.by_type.insert("testType".to_string(), 0);

        // First render: initial process hangs, respawns, then succeeds.
        let r1 = mgr.render("testType", &serde_json::json!({"a": 1}));
        assert!(
            matches!(r1, Some(PluginRender::Lines(_))),
            "first render should succeed after respawn"
        );

        // Second render (different key to avoid cache): uses the respawned process.
        let r2 = mgr.render("testType", &serde_json::json!({"b": 2}));
        assert!(
            matches!(r2, Some(PluginRender::Lines(_))),
            "second render should succeed on respawned process"
        );
    }
}
