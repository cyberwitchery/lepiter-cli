//! external snippet renderer integration via json ipc.
//!
//! plugins are configured with `LEPITER_PLUGIN_CONFIG` and receive snippet
//! render requests over stdin/stdout as json lines.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use lepiter_core::plugin::{PluginRequest, PluginResponse};

use crate::util::{cache_limit_from_env, cache_limit_from_env_allow_zero};

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

struct PluginProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
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
        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
        })
    }

    fn request_with_timeout(
        &mut self,
        req: &PluginRequest,
        timeout: Duration,
    ) -> Result<PluginResponse> {
        if let Some(status) = self.child.try_wait()? {
            bail!("plugin exited: {status}");
        }
        let mut payload = serde_json::to_string(req)?;
        payload.push('\n');
        self.stdin.write_all(payload.as_bytes())?;
        self.stdin.flush()?;

        let start = Instant::now();
        loop {
            if start.elapsed() >= timeout {
                bail!("plugin timeout after {}ms", timeout.as_millis());
            }
            if let Some(status) = self.child.try_wait()? {
                bail!("plugin exited: {status}");
            }
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                continue;
            }
            let resp: PluginResponse = serde_json::from_str(&line)?;
            return Ok(resp);
        }
    }
}

struct PluginHandle {
    name: String,
    process: PluginProcess,
}

pub struct PluginManager {
    processes: Vec<PluginHandle>,
    by_type: HashMap<String, usize>,
    cache: HashMap<(String, u64), PluginRender>,
    cache_order: VecDeque<(String, u64)>,
    max_cache: usize,
    timeout: Duration,
    retries: usize,
    notes: Vec<String>,
}

impl PluginManager {
    pub fn empty() -> Self {
        Self {
            processes: Vec::new(),
            by_type: HashMap::new(),
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            max_cache: cache_limit_from_env_allow_zero("LEPITER_PLUGIN_CACHE", 128),
            timeout: Duration::from_millis(
                std::env::var("LEPITER_PLUGIN_TIMEOUT_MS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(250),
            ),
            retries: std::env::var("LEPITER_PLUGIN_RETRIES")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1),
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
                return Self {
                    processes: Vec::new(),
                    by_type: HashMap::new(),
                    cache: HashMap::new(),
                    cache_order: VecDeque::new(),
                    max_cache: cache_limit_from_env_allow_zero("LEPITER_PLUGIN_CACHE", 128),
                    timeout: Duration::from_millis(
                        std::env::var("LEPITER_PLUGIN_TIMEOUT_MS")
                            .ok()
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(250),
                    ),
                    retries: std::env::var("LEPITER_PLUGIN_RETRIES")
                        .ok()
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(1),
                    notes: vec![format!("plugin config read failed: {err}")],
                };
            }
        };
        let config: PluginConfig = match serde_json::from_slice(&bytes) {
            Ok(config) => config,
            Err(err) => {
                return Self {
                    processes: Vec::new(),
                    by_type: HashMap::new(),
                    cache: HashMap::new(),
                    cache_order: VecDeque::new(),
                    max_cache: cache_limit_from_env("LEPITER_PLUGIN_CACHE", 128),
                    timeout: Duration::from_millis(
                        std::env::var("LEPITER_PLUGIN_TIMEOUT_MS")
                            .ok()
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(250),
                    ),
                    retries: std::env::var("LEPITER_PLUGIN_RETRIES")
                        .ok()
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(1),
                    notes: vec![format!("plugin config parse failed: {err}")],
                };
            }
        };

        let mut processes = Vec::new();
        let mut by_type = HashMap::new();
        let mut notes = Vec::new();
        for plugin in config.plugins {
            let process = match spawn_plugin_process(&plugin) {
                Ok(process) => process,
                Err(err) => {
                    notes.push(format!("plugin {} failed to start: {err}", plugin.name));
                    continue;
                }
            };
            let idx = processes.len();
            processes.push(PluginHandle {
                name: plugin.name.clone(),
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
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            max_cache: cache_limit_from_env_allow_zero("LEPITER_PLUGIN_CACHE", 128),
            timeout: Duration::from_millis(
                std::env::var("LEPITER_PLUGIN_TIMEOUT_MS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(250),
            ),
            retries: std::env::var("LEPITER_PLUGIN_RETRIES")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1),
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
        let handle = self.processes.get_mut(idx)?;

        let key = (typ.to_string(), hash_value(raw));
        if let Some(hit) = self.cache.get(&key).cloned() {
            touch_cache_lru(&mut self.cache_order, &key);
            return Some(hit);
        }

        let req = PluginRequest {
            typ: typ.to_string(),
            snippet: raw.clone(),
        };
        let mut last_err = None;
        for _ in 0..=self.retries {
            match handle.process.request_with_timeout(&req, self.timeout) {
                Ok(resp) => {
                    let rendered = if resp.ok {
                        PluginRender::Lines(resp.lines)
                    } else {
                        let msg = resp
                            .error
                            .unwrap_or_else(|| "plugin returned error".to_string());
                        PluginRender::Error(format!("{}: {}", handle.name, msg))
                    };
                    self.insert_cache(key.clone(), rendered.clone());
                    return Some(rendered);
                }
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }
        let err = last_err.unwrap_or_else(|| anyhow::anyhow!("plugin failed"));
        let rendered = PluginRender::Error(format!("{}: {err}", handle.name));
        self.insert_cache(key, rendered.clone());
        Some(rendered)
    }

    pub fn status_line(&self) -> String {
        let mut out = format!(
            "plugins: {} | cache: {}/{} | timeout: {}ms | retries: {}",
            self.processes.len(),
            self.cache.len(),
            self.max_cache,
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

    fn insert_cache(&mut self, key: (String, u64), value: PluginRender) {
        if self.max_cache == 0 {
            return;
        }
        if self.cache.contains_key(&key) {
            self.cache.insert(key.clone(), value);
            touch_cache_lru(&mut self.cache_order, &key);
            return;
        }
        if self.cache.len() >= self.max_cache
            && let Some(oldest) = self.cache_order.pop_front()
        {
            self.cache.remove(&oldest);
        }
        self.cache_order.push_back(key.clone());
        self.cache.insert(key, value);
    }
}

fn touch_cache_lru(order: &mut VecDeque<(String, u64)>, key: &(String, u64)) {
    if let Some(pos) = order.iter().position(|k| k == key) {
        order.remove(pos);
    }
    order.push_back(key.clone());
}

fn spawn_plugin_process(plugin: &PluginSpec) -> Result<PluginProcess> {
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
            Ok(process) => return Ok(process),
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
