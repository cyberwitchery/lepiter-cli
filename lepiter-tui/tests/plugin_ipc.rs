use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

#[test]
fn wardley_plugin_renders_nodes_over_ipc() -> Result<()> {
    let binary = build_plugin_binary()?;
    let snippet = sample_wardley_snippet();

    let req = serde_json::json!({
        "type": "wardleyMap",
        "snippet": snippet,
    });
    let output = std::process::Command::new(binary)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            let payload = format!("{}\n", serde_json::to_string(&req)?);
            stdin.write_all(payload.as_bytes())?;
            drop(stdin);
            let output = child.wait_with_output()?;
            Ok(output)
        })?;

    let line = String::from_utf8_lossy(&output.stdout);
    let resp: serde_json::Value = serde_json::from_str(line.trim())?;
    assert!(resp.get("ok").and_then(Value::as_bool).unwrap_or(false));
    let lines = resp
        .get("lines")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(!lines.is_empty());
    Ok(())
}

fn build_plugin_binary() -> Result<PathBuf> {
    let workspace_root = workspace_root()?;
    let status = std::process::Command::new("cargo")
        .args(["build", "-p", "lepiter-core", "--example", "wardley_plugin"])
        .current_dir(&workspace_root)
        .status()
        .context("failed to invoke cargo")?;
    if !status.success() {
        anyhow::bail!("failed to build wardley_plugin example");
    }
    let mut path = workspace_root;
    path.push("target");
    path.push("debug");
    path.push("examples");
    path.push(exe_name("wardley_plugin"));
    if !path.exists() {
        anyhow::bail!("wardley_plugin not found at {}", path.display());
    }
    Ok(path)
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .context("failed to resolve workspace root")?;
    Ok(root.to_path_buf())
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn sample_wardley_snippet() -> Value {
    serde_json::json!({
        "__type": "wardleyMap",
        "wardleyMapDictionary": {
            "nodes": [
                { "label": { "text": "alpha" } },
                { "label": { "text": "beta" } }
            ]
        }
    })
}
