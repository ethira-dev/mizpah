//! Reassemble NestJS / util.inspect-style pretty object dumps into JSON.

const MAX_BUFFER_LINES: usize = 256;

/// Per-service accumulator for multiline `{` … `}` pretty blocks.
#[derive(Debug, Default)]
pub struct PrettyBuffer {
    lines: Vec<String>,
    depth: i32,
}

impl PrettyBuffer {
    pub fn start(first_line: String) -> Self {
        let depth = brace_depth_delta(&first_line);
        Self {
            lines: vec![first_line],
            depth,
        }
    }

    pub fn push(&mut self, line: String) {
        self.depth += brace_depth_delta(&line);
        self.lines.push(line);
    }

    pub fn is_complete(&self) -> bool {
        self.depth <= 0 && !self.lines.is_empty()
    }

    pub fn is_oversized(&self) -> bool {
        self.lines.len() >= MAX_BUFFER_LINES
    }

    pub fn into_lines(self) -> Vec<String> {
        self.lines
    }

    pub fn joined(&self) -> String {
        self.lines.join("\n")
    }
}

/// Strip common ANSI CSI / OSC sequences so brace/key detection works.
pub fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                b'[' => {
                    i += 1;
                    while i < bytes.len() {
                        let c = bytes[i];
                        i += 1;
                        if c.is_ascii_uppercase() || c.is_ascii_lowercase() {
                            break;
                        }
                    }
                }
                b']' => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    // Skip single-char ESC sequences; drop the ESC itself.
                }
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Remove a leading `[service]` tag when it matches the ingest service name.
///
/// Upstream pipes sometimes echo the service name on every line (e.g. `[api] {`),
/// which would otherwise prevent pretty-block detection.
pub fn strip_service_prefix(line: &str, service: &str) -> String {
    if service.is_empty() {
        return line.to_string();
    }
    let trimmed_start = line.trim_start();
    let prefix = format!("[{service}]");
    if let Some(rest) = trimmed_start.strip_prefix(&prefix) {
        rest.trim_start().to_string()
    } else {
        line.to_string()
    }
}

/// True when the cleaned line is a bare object opener that should start buffering.
pub fn is_pretty_block_start(cleaned: &str) -> bool {
    cleaned.trim() == "{"
}

/// Try to parse a completed pretty block into a JSON object.
pub fn parse_pretty_block(joined: &str) -> Option<serde_json::Value> {
    let jsonish = js_literal_to_json(joined)?;
    match serde_json::from_str::<serde_json::Value>(&jsonish) {
        Ok(serde_json::Value::Object(obj)) => Some(serde_json::Value::Object(obj)),
        _ => None,
    }
}

/// Convert a JS-object-literal dump into JSON text.
///
/// Handles: unquoted keys, single-quoted strings, `undefined` → `null`, trailing commas.
pub fn js_literal_to_json(input: &str) -> Option<String> {
    let mut out = String::with_capacity(input.len() + 16);
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut in_double = false;
    let mut in_single = false;
    let mut escape = false;

    while i < chars.len() {
        let c = chars[i];

        if in_double {
            if escape {
                out.push(c);
                escape = false;
            } else if c == '\\' {
                out.push(c);
                escape = true;
            } else if c == '"' {
                out.push(c);
                in_double = false;
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }

        if in_single {
            if escape {
                match c {
                    '\'' => out.push('\''),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '\'' {
                out.push('"');
                in_single = false;
            } else if c == '"' {
                out.push('\\');
                out.push('"');
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }

        match c {
            '"' => {
                out.push(c);
                in_double = true;
                i += 1;
            }
            '\'' => {
                out.push('"');
                in_single = true;
                i += 1;
            }
            '/' if i + 1 < chars.len() && chars[i + 1] == '/' => {
                // Line comment — skip to end of line
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            // Trailing comma before } or ]
            ',' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                    i += 1;
                    continue;
                }
                out.push(',');
                i += 1;
            }
            // `undefined` as a bare value
            'u' if matches_keyword(&chars, i, "undefined") => {
                out.push_str("null");
                i += "undefined".len();
            }
            // Unquoted identifier key: ident followed by optional whitespace and ':'
            c if is_ident_start(c) => {
                let start = i;
                i += 1;
                while i < chars.len() && is_ident_continue(chars[i]) {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ':' {
                    out.push('"');
                    out.push_str(&ident);
                    out.push('"');
                } else if ident == "true" || ident == "false" || ident == "null" {
                    out.push_str(&ident);
                } else if ident == "undefined" {
                    out.push_str("null");
                } else {
                    // Bare identifier as a value — quote it
                    out.push('"');
                    out.push_str(&ident);
                    out.push('"');
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }

    if in_double || in_single {
        return None;
    }
    Some(out)
}

fn matches_keyword(chars: &[char], i: usize, word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    if i + w.len() > chars.len() {
        return false;
    }
    if chars[i..i + w.len()] != w[..] {
        return false;
    }
    // Not part of a longer identifier
    let before_ok = i == 0 || !is_ident_continue(chars[i - 1]);
    let after_ok = i + w.len() >= chars.len() || !is_ident_continue(chars[i + w.len()]);
    before_ok && after_ok
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Net change in `{`/`}` depth for one line, ignoring braces inside quotes.
fn brace_depth_delta(line: &str) -> i32 {
    let mut depth = 0i32;
    let mut in_double = false;
    let mut in_single = false;
    let mut escape = false;
    for c in line.chars() {
        if in_double {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_double = false;
            }
            continue;
        }
        if in_single {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '\'' {
                in_single = false;
            }
            continue;
        }
        match c {
            '"' => in_double = true,
            '\'' => in_single = true,
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_nestjs_style_block() {
        let block = r#"{
  userId: undefined,
  workspaceId: 'c0f00a4b-20d8-4599-89bd-d752101a4855',
  correlationId: 'job-3653b3d7-5109-4f3e-b748-d0de76d92179',
  ms: '+0ms',
  timestamp: '2026-07-15T19:37:27.513Z',
  message: 'Running repository data scan for ethira-dev/monorepo (workspace c0f00a4b-20d8-4599-89bd-d752101a4855)',
  level: 'info',
  context: 'CursorAgentService',
}"#;
        let value = parse_pretty_block(block).expect("should parse");
        assert_eq!(value["level"], json!("info"));
        assert_eq!(value["context"], json!("CursorAgentService"));
        assert_eq!(value["userId"], json!(null));
        assert_eq!(
            value["workspaceId"],
            json!("c0f00a4b-20d8-4599-89bd-d752101a4855")
        );
    }

    #[test]
    fn nested_object_and_array() {
        let block = r#"{
  meta: {
    tags: ['a', 'b'],
    count: 2,
  },
  ok: true,
}"#;
        let value = parse_pretty_block(block).expect("should parse");
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["meta"]["count"], json!(2));
        assert_eq!(value["meta"]["tags"], json!(["a", "b"]));
    }

    #[test]
    fn strip_ansi_csi() {
        let raw = "\x1b[32m{\x1b[0m";
        assert_eq!(strip_ansi(raw).trim(), "{");
    }

    #[test]
    fn buffer_completes_on_matching_braces() {
        let mut buf = PrettyBuffer::start("{".into());
        assert!(!buf.is_complete());
        buf.push("  a: 1,".into());
        assert!(!buf.is_complete());
        buf.push("}".into());
        assert!(buf.is_complete());
        let value = parse_pretty_block(&buf.joined()).unwrap();
        assert_eq!(value["a"], json!(1));
    }

    #[test]
    fn single_quoted_with_double_inside() {
        let block = "{ msg: 'say \"hi\"', }";
        let value = parse_pretty_block(block).unwrap();
        assert_eq!(value["msg"], json!("say \"hi\""));
    }

    #[test]
    fn pretty_json_also_works() {
        let block = r#"{
  "level": "info",
  "message": "hello"
}"#;
        let value = parse_pretty_block(block).unwrap();
        assert_eq!(value["message"], json!("hello"));
    }

    #[test]
    fn strip_service_prefix_matches() {
        assert_eq!(strip_service_prefix("[api] {", "api"), "{");
        assert_eq!(
            strip_service_prefix("[api]   ms: '+0ms'", "api"),
            "ms: '+0ms'"
        );
        assert_eq!(
            strip_service_prefix("[api]   context: { context: 'bootstrap' },", "api"),
            "context: { context: 'bootstrap' },"
        );
    }

    #[test]
    fn strip_service_prefix_ignores_other_tags() {
        assert_eq!(strip_service_prefix("[other] {", "api"), "[other] {");
        assert_eq!(strip_service_prefix("{", "api"), "{");
    }

    #[test]
    fn last_property_without_trailing_comma() {
        let block = r#"{
  level: 'info',
  ms: '+0ms'
}"#;
        let value = parse_pretty_block(block).expect("should parse");
        assert_eq!(value["level"], json!("info"));
        assert_eq!(value["ms"], json!("+0ms"));
    }
}
