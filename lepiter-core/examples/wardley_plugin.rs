use lepiter_core::lepiter_plugin_main;
use lepiter_core::plugin::{PluginRequest, PluginResponse};
use serde_json::Value;

fn render_request(req: PluginRequest) -> PluginResponse {
    if req.typ != "wardleyMap" {
        return PluginResponse::error(format!("unsupported type: {}", req.typ));
    }

    let nodes = extract_wardley_nodes(&req.snippet);
    let mut lines = Vec::new();
    lines.push(format!("wardley map ({} nodes)", nodes.len()));
    if nodes.is_empty() {
        lines.push("<no nodes found>".to_string());
    } else {
        for name in nodes {
            lines.push(format!("- {name}"));
        }
    }

    PluginResponse::ok(lines)
}

fn extract_wardley_nodes(snippet: &Value) -> Vec<String> {
    let Some(nodes) = snippet
        .get("wardleyMapDictionary")
        .and_then(|v| v.get("nodes"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    nodes
        .iter()
        .filter_map(|node| {
            node.get("label")
                .and_then(|label| label.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

lepiter_plugin_main!(render_request);
