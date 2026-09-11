//! Tool display metadata shared by `run` and the TUI: the per-tool inline icon
//! and a title summarizing a tool call's arguments (the reference CLI's `run/tool.ts`).

use serde_json::Value;

/// The inline glyph for a tool (the reference CLI's tool-icon set).
pub fn tool_icon(name: &str) -> &'static str {
    match name {
        "Read" => "→",
        "Write" | "Edit" | "MultiEdit" | "ApplyPatch" => "←",
        "Bash" | "ShellBash" => "$",
        "Glob" | "Grep" => "✱",
        "WebFetch" => "%",
        "WebSearch" => "◈",
        "Task" => "✓",
        "NotebookEdit" => "✎",
        "TodoWrite" => "☐",
        "Question" => "?",
        _ => "⚙",
    }
}

/// Summarize a tool call's JSON-stringified input as `[key=value, ...]` for
/// primitive fields (the reference CLI's inline `[args]` summary).
pub fn summarize_input(input: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(input) else {
        return String::new();
    };
    let Value::Object(obj) = value else {
        return String::new();
    };
    let parts: Vec<String> = obj
        .iter()
        .filter(|(_, v)| v.is_string() || v.is_number() || v.is_boolean())
        .map(|(k, v)| {
            let rendered = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{k}={rendered}")
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" [{}]", parts.join(", "))
    }
}
