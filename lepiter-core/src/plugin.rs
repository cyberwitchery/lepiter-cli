//! plugin sdk for external snippet renderers.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// ipc request sent to an external snippet plugin.
#[derive(Debug, Deserialize, Serialize)]
pub struct PluginRequest {
    /// snippet type (e.g. `wardleyMap`).
    #[serde(rename = "type")]
    pub typ: String,
    /// raw snippet json.
    pub snippet: Value,
}

/// ipc response returned by an external snippet plugin.
#[derive(Debug, Serialize, Deserialize)]
pub struct PluginResponse {
    /// whether the render succeeded.
    pub ok: bool,
    /// rendered text lines.
    pub lines: Vec<String>,
    /// optional error message.
    pub error: Option<String>,
}

impl PluginResponse {
    /// successful response with rendered lines.
    pub fn ok(lines: Vec<String>) -> Self {
        Self {
            ok: true,
            lines,
            error: None,
        }
    }

    /// error response with message.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            lines: Vec::new(),
            error: Some(message.into()),
        }
    }
}

/// runs a newline-delimited json plugin loop over the given reader and writer.
///
/// the handler is invoked once per request. responses are serialized back to the writer.
/// use [`plugin_loop`] for the common case of stdin/stdout.
pub fn plugin_loop_io<R, W, F>(reader: R, mut writer: W, mut handler: F) -> io::Result<()>
where
    R: BufRead,
    W: Write,
    F: FnMut(PluginRequest) -> PluginResponse,
{
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Result<PluginRequest, _> = serde_json::from_str(&line);
        let resp = match req {
            Ok(req) => handler(req),
            Err(err) => PluginResponse::error(format!("invalid request: {err}")),
        };
        let json = serde_json::to_string(&resp).unwrap_or_else(|_| {
            "{\"ok\":false,\"lines\":[],\"error\":\"encode failed\"}".to_string()
        });
        writeln!(writer, "{json}")?;
        writer.flush()?;
    }
    Ok(())
}

/// runs a newline-delimited json plugin loop on stdin/stdout.
///
/// convenience wrapper around [`plugin_loop_io`].
pub fn plugin_loop<F>(handler: F) -> io::Result<()>
where
    F: FnMut(PluginRequest) -> PluginResponse,
{
    let stdin = io::stdin();
    let reader = stdin.lock();
    let writer = io::BufWriter::new(io::stdout());
    plugin_loop_io(reader, writer, handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn run_loop(input: &str, handler: impl FnMut(PluginRequest) -> PluginResponse) -> String {
        let reader = Cursor::new(input.as_bytes().to_vec());
        let mut output = Vec::new();
        plugin_loop_io(reader, &mut output, handler).unwrap();
        String::from_utf8(output).unwrap()
    }

    fn echo_handler(req: PluginRequest) -> PluginResponse {
        PluginResponse::ok(vec![format!("echo: {}", req.typ)])
    }

    #[test]
    fn valid_request() {
        let input = "{\"type\":\"wardleyMap\",\"snippet\":{}}\n";
        let output = run_loop(input, echo_handler);
        let resp: PluginResponse = serde_json::from_str(output.trim()).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.lines, vec!["echo: wardleyMap"]);
        assert!(resp.error.is_none());
    }

    #[test]
    fn malformed_json() {
        let input = "not valid json\n";
        let output = run_loop(input, echo_handler);
        let resp: PluginResponse = serde_json::from_str(output.trim()).unwrap();
        assert!(!resp.ok);
        assert!(resp.error.as_ref().unwrap().contains("invalid request"));
    }

    #[test]
    fn empty_lines_skipped() {
        let input = "\n   \n{\"type\":\"a\",\"snippet\":null}\n\n";
        let output = run_loop(input, echo_handler);
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(lines.len(), 1);
        let resp: PluginResponse = serde_json::from_str(lines[0]).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.lines, vec!["echo: a"]);
    }

    #[test]
    fn handler_error_propagated() {
        let input = "{\"type\":\"x\",\"snippet\":{}}\n";
        let output = run_loop(input, |_| PluginResponse::error("boom"));
        let resp: PluginResponse = serde_json::from_str(output.trim()).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("boom"));
        assert!(resp.lines.is_empty());
    }

    #[test]
    fn serialization_roundtrip() {
        let req = PluginRequest {
            typ: "code".to_string(),
            snippet: serde_json::json!({"lang": "rust"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: PluginRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.typ, "code");
        assert_eq!(decoded.snippet["lang"], "rust");

        let resp = PluginResponse::ok(vec!["line1".into(), "line2".into()]);
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: PluginResponse = serde_json::from_str(&json).unwrap();
        assert!(decoded.ok);
        assert_eq!(decoded.lines, vec!["line1", "line2"]);
        assert!(decoded.error.is_none());
    }

    #[test]
    fn multiple_requests() {
        let input = "{\"type\":\"a\",\"snippet\":{}}\n{\"type\":\"b\",\"snippet\":{}}\n";
        let mut calls = 0usize;
        let output = run_loop(input, |req| {
            calls += 1;
            PluginResponse::ok(vec![req.typ])
        });
        let lines: Vec<PluginResponse> = output
            .trim()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].lines, vec!["a"]);
        assert_eq!(lines[1].lines, vec!["b"]);
    }
}
