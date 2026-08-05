//! external snippet renderer integration via json ipc.
//!
//! plugins are configured with `LEPITER_PLUGIN_CONFIG` and receive snippet
//! render requests over stdin/stdout as json lines.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
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

/// Distinguishes timeout errors from other plugin failures so the retry
/// loop can decide whether to kill-and-respawn.
#[derive(Debug)]
enum RequestError {
    /// Plugin did not respond within the deadline.
    Timeout(Duration),
    /// Plugin process died, produced bad output, or hit an I/O error.
    Failed(anyhow::Error),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::Timeout(d) => write!(f, "timed out after {}ms", d.as_millis()),
            RequestError::Failed(e) => write!(f, "{e}"),
        }
    }
}

struct PluginProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    /// Receives lines read by a background reader thread.
    rx: mpsc::Receiver<io::Result<String>>,
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl PluginProcess {
    fn spawn(binary: &str, args: &[String]) -> Result<Self> {
        let mut cmd = Command::new(binary);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().with_context(|| format!("spawn {binary}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing plugin stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing plugin stdout"))?;

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
            return Err(RequestError::Failed(anyhow::anyhow!(
                "plugin exited: {status}"
            )));
        }

        let mut payload = serde_json::to_string(req).map_err(|e| RequestError::Failed(e.into()))?;
        payload.push('\n');
        self.stdin
            .write_all(payload.as_bytes())
            .map_err(|e| RequestError::Failed(e.into()))?;
        self.stdin
            .flush()
            .map_err(|e| RequestError::Failed(e.into()))?;

        match self.rx.recv_timeout(timeout) {
            Ok(Ok(line)) => {
                let resp: PluginResponse =
                    serde_json::from_str(&line).map_err(|e| RequestError::Failed(e.into()))?;
                Ok(resp)
            }
            Ok(Err(io_err)) => Err(RequestError::Failed(anyhow::anyhow!(
                "plugin stdout error: {io_err}"
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(RequestError::Timeout(timeout)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(RequestError::Failed(
                anyhow::anyhow!("plugin reader thread terminated unexpectedly"),
            )),
        }
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
    notes: Vec<String>,
}

impl PluginManager {
    fn env_defaults() -> (LruCache<(String, u64), PluginRender>, Duration, usize) {
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
        (cache, timeout, retries)
    }

    pub fn empty() -> Self {
        let (cache, timeout, retries) = Self::env_defaults();
        Self {
            processes: Vec::new(),
            by_type: HashMap::new(),
            cache,
            timeout,
            retries,
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

        let mut processes = Vec::new();
        let mut by_type = HashMap::new();
        let mut notes = Vec::new();
        for plugin in config.plugins {
            let (process, binary) = match spawn_plugin_process(&plugin) {
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

        let (cache, timeout, retries) = Self::env_defaults();
        Self {
            processes,
            by_type,
            cache,
            timeout,
            retries,
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
        let mut last_err = None;
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
                    last_err = Some(format!("{e}"));
                    // Kill the hung/dead plugin and try to respawn for the next
                    // retry attempt. Respawning is essential after a timeout
                    // because the old channel may contain a stale response.
                    let binary = self.processes[idx].binary.clone();
                    let args = self.processes[idx].args.clone();
                    match PluginProcess::spawn(&binary, &args) {
                        Ok(new_process) => {
                            self.processes[idx].process = new_process;
                        }
                        Err(spawn_err) => {
                            last_err = Some(format!("respawn failed: {spawn_err}"));
                            break;
                        }
                    }
                }
            }
        }
        let err = last_err.unwrap_or_else(|| "plugin failed".to_string());
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
fn spawn_plugin_process(plugin: &PluginSpec) -> Result<(PluginProcess, String)> {
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
        match PluginProcess::spawn(&candidate, &plugin.args) {
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
        PluginProcess::spawn("bash", &["-c".to_string(), script.to_string()]).unwrap()
    }

    #[test]
    fn working_plugin_responds_within_timeout() {
        let script = r#"read line; echo '{"ok":true,"lines":["hello"],"error":null}'"#;
        let mut proc =
            PluginProcess::spawn("bash", &["-c".to_string(), script.to_string()]).unwrap();
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
        let mut proc = PluginProcess::spawn("sleep", &["60".to_string()]).unwrap();
        let req = dummy_request();
        let start = std::time::Instant::now();
        let result = proc.request_with_timeout(&req, Duration::from_millis(100));
        let elapsed = start.elapsed();
        match result {
            Err(RequestError::Timeout(d)) => {
                assert_eq!(d, Duration::from_millis(100));
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
        let mut proc =
            PluginProcess::spawn("bash", &["-c".to_string(), script.to_string()]).unwrap();
        let req = dummy_request();
        let result = proc.request_with_timeout(&req, Duration::from_secs(5));
        assert!(
            matches!(result, Err(RequestError::Failed(_))),
            "expected Failed, got: {result:?}"
        );
    }

    #[test]
    fn request_detects_already_exited_plugin() {
        let mut proc = PluginProcess::spawn("true", &[]).unwrap();
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
        let proc = PluginProcess::spawn("bash", &["-c".to_string(), script.to_string()]).unwrap();

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
