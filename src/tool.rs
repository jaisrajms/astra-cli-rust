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

/// The reference CLI's per-tool inline label: the tool name phrased around its "primary" argument
/// (e.g. `Read src/main.rs`, `Glob "*.rs" in src`, `$ ls`), falling back to the `[key=value, ...]`
/// summary for tools without a dedicated label.
pub fn tool_label(name: &str, input: &str) -> String {
    let value = serde_json::from_str::<Value>(input).unwrap_or(Value::Null);

    fn field(value: &Value, key: &str) -> Option<String> {
        match value.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::Bool(b)) => Some(b.to_string()),
            _ => None,
        }
    }

    let in_dir = |dir: Option<String>| match dir {
        Some(d) if !d.is_empty() && d != "." => Some(d),
        _ => None,
    };

    match name {
        "Read" => format!("Read {}", field(&value, "file_path").unwrap_or_default()),
        "Write" => format!("Write {}", field(&value, "file_path").unwrap_or_default()),
        "Edit" | "MultiEdit" => format!("Edit {}", field(&value, "file_path").unwrap_or_default()),
        "NotebookEdit" => format!(
            "Edit {}",
            field(&value, "notebook_path").unwrap_or_default()
        ),
        "Bash" | "ShellBash" => field(&value, "command").unwrap_or_default(),
        "Glob" => match in_dir(field(&value, "path")) {
            Some(dir) => format!(
                "Glob \"{}\" in {dir}",
                field(&value, "pattern").unwrap_or_default()
            ),
            None => format!("Glob \"{}\"", field(&value, "pattern").unwrap_or_default()),
        },
        "Grep" => match in_dir(field(&value, "path")) {
            Some(dir) => format!(
                "Grep \"{}\" in {dir}",
                field(&value, "pattern").unwrap_or_default()
            ),
            None => format!("Grep \"{}\"", field(&value, "pattern").unwrap_or_default()),
        },
        "WebFetch" => format!("WebFetch {}", field(&value, "url").unwrap_or_default()),
        "WebSearch" => format!(
            "WebSearch \"{}\"",
            field(&value, "query").unwrap_or_default()
        ),
        "TodoWrite" => "Updating todos…".to_string(),
        "Task" => {
            let sub = field(&value, "subagent_type").unwrap_or_else(|| "General".to_string());
            let desc = field(&value, "description").unwrap_or_default();
            format!("{sub} Task — {desc}")
        }
        _ => format!("{name}{}", summarize_input(input)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_label_phrases_the_primary_argument() {
        assert_eq!(
            tool_label("Read", r#"{"file_path":"src/main.rs"}"#),
            "Read src/main.rs"
        );
        assert_eq!(tool_label("Bash", r#"{"command":"ls -la"}"#), "ls -la");
        assert_eq!(
            tool_label("Glob", r#"{"pattern":"*.rs","path":"src"}"#),
            "Glob \"*.rs\" in src"
        );
        assert_eq!(tool_label("Glob", r#"{"pattern":"*.rs"}"#), "Glob \"*.rs\"");
        assert_eq!(tool_label("Edit", r#"{"file_path":"a.rs"}"#), "Edit a.rs");
        assert_eq!(
            tool_label("TodoWrite", r#"{"todos":[]}"#),
            "Updating todos…"
        );
    }

    #[test]
    fn tool_label_falls_back_to_arg_summary() {
        assert_eq!(
            tool_label("UnknownTool", r#"{"a":1,"b":"x"}"#),
            "UnknownTool [a=1, b=x]"
        );
        assert_eq!(tool_label("UnknownTool", r#"{}"#), "UnknownTool");
    }

    #[test]
    fn tool_label_handles_malformed_input() {
        assert_eq!(tool_label("Read", "not json"), "Read ");
        assert_eq!(tool_label("Bash", ""), "");
    }
}
