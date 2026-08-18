# plugins

lepiter-cli supports external snippet renderers over a simple ipc protocol.
plugins handle unknown snippet types (for example `wardleyMap`) and return
rendered lines to display in the tui.

## overview

- plugins are long-lived processes (spawned once, reused per render)
- plugins are resolved via `LEPITER_PLUGIN_CONFIG`
- plugins only run for `Node::Unknown` types
- failures fall back to `[[unknown: <type>]]`

## config

set `LEPITER_PLUGIN_CONFIG` to a json file:

```json
{
  "plugins": [
    {
      "name": "wardley",
      "types": ["wardleyMap"]
    }
  ]
}
```

`name` is used to find the plugin binary on `PATH`. if `binary` is not
provided, the tui tries:

1. `lepiter-plugin-<name>`
2. `lepiter-<name>`
3. `<name>`

optional fields:

- `binary`: explicit executable path
- `args`: extra arguments passed to the plugin process

## protocol

the tui communicates over stdin/stdout using newline-delimited json.

request:

```json
{ "type": "wardleyMap", "snippet": { ... } }
```

response:

```json
{ "ok": true, "lines": ["line one", "line two"], "error": null }
```

on error:

```json
{ "ok": false, "lines": [], "error": "reason" }
```

stdout is reserved for the protocol. write diagnostics to stderr instead: the
tail of what a plugin writes there is appended to the error the tui reports
when that plugin crashes, exits, times out, or answers with an unreadable
line.

## demo plugin

the repository ships a minimal example:

```bash
cargo build -p lepiter-core --example wardley_plugin
```

use it by putting a config file on `LEPITER_PLUGIN_CONFIG`, for example:

```json
{
  "plugins": [
    {
      "name": "wardley",
      "binary": "target/debug/examples/wardley_plugin",
      "types": ["wardleyMap"]
    }
  ]
}
```

## plugin sdk

`lepiter-core` exposes types and a macro to reduce boilerplate:

```rust
use lepiter_core::plugin::{PluginRequest, PluginResponse};
use lepiter_core::lepiter_plugin_main;

fn handle(req: PluginRequest) -> PluginResponse {
    if req.typ != "wardleyMap" {
        return PluginResponse::error("unsupported type");
    }
    PluginResponse::ok(vec!["example".to_string()])
}

lepiter_plugin_main!(handle);
```

## performance

- plugins are reused across snippet renders
- no per-snippet process spawn
- render output is cached by snippet hash

## runtime settings

- `LEPITER_PLUGIN_CONFIG`: json config file (required to enable plugins)
- `LEPITER_PLUGIN_CACHE`: max cached render entries (default `128`, `0` disables)
- `LEPITER_PLUGIN_TIMEOUT_MS`: per-request timeout (default `250`)
- `LEPITER_PLUGIN_RETRIES`: retries after failures (default `1`)
- `LEPITER_PLUGIN_STDERR_BYTES`: bytes of plugin stderr kept for error reports
  (default `2048`, `0` discards stderr)

## limitations

- plugins cannot render inline text styles (tui treats returned lines as plain text)
- plugins only run for unknown snippet types
- plugin stderr is only kept as a bounded tail, and only surfaces when a render
  fails (stdout is reserved for protocol)
