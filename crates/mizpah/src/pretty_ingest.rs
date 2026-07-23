//! Reassemble NestJS / util.inspect-style pretty object dumps into JSON.
//!
//! Recovery is gated and fused: valid NDJSON never pays for repair; messy dumps get
//! one rewrite pass + bounded salvage, then a single joined `_raw` as last resort.

use serde_json::{Map, Value};

/// Max lines in a pretty buffer before forced flush.
const MAX_BUFFER_LINES: usize = 4096;
/// Max accumulated UTF-8 bytes in a pretty buffer before forced flush.
const MAX_BUFFER_BYTES: usize = 1024 * 1024;
/// Max salvage parse attempts after fused repair fails.
const MAX_SALVAGE_ATTEMPTS: usize = 3;

/// Test-only: how many times the fused repair scanner allocated an output buffer.
#[cfg(test)]
mod repair_stats {
    use std::sync::atomic::{AtomicU64, Ordering};
    static REPAIR_SCAN_COUNT: AtomicU64 = AtomicU64::new(0);

    pub fn count() -> u64 {
        REPAIR_SCAN_COUNT.load(Ordering::Relaxed)
    }

    pub fn reset() {
        REPAIR_SCAN_COUNT.store(0, Ordering::Relaxed);
    }

    pub fn bump() {
        REPAIR_SCAN_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
pub fn repair_scan_count() -> u64 {
    repair_stats::count()
}

#[cfg(test)]
pub fn reset_repair_scan_count() {
    repair_stats::reset();
}

/// Per-service accumulator for multiline `{` … `}` pretty blocks.
#[derive(Debug, Default)]
pub struct PrettyBuffer {
    lines: Vec<String>,
    depth: i32,
    byte_len: usize,
}

impl PrettyBuffer {
    pub fn start(first_line: String) -> Self {
        let depth = brace_depth_delta(&first_line);
        let byte_len = first_line.len();
        Self {
            lines: vec![first_line],
            depth,
            byte_len,
        }
    }

    pub fn push(&mut self, line: String) {
        self.depth += brace_depth_delta(&line);
        // +1 for the join newline that `joined()` will insert between lines.
        self.byte_len = self.byte_len.saturating_add(1).saturating_add(line.len());
        self.lines.push(line);
    }

    pub fn is_complete(&self) -> bool {
        self.depth <= 0 && !self.lines.is_empty()
    }

    pub fn is_oversized(&self) -> bool {
        self.lines.len() >= MAX_BUFFER_LINES || self.byte_len >= MAX_BUFFER_BYTES
    }

    pub fn into_joined(self) -> String {
        self.lines.join("\n")
    }

    #[cfg(test)]
    pub fn into_lines(self) -> Vec<String> {
        self.lines
    }

    #[cfg(test)]
    pub fn joined(&self) -> String {
        self.lines.join("\n")
    }

    #[cfg(test)]
    pub fn byte_len(&self) -> usize {
        self.byte_len
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
        // Copy contiguous UTF-8 spans intact (byte-as-char would Latin-1-decode).
        let start = i;
        while i < bytes.len() && bytes[i] != 0x1b {
            i += 1;
        }
        out.push_str(&input[start..i]);
    }
    out
}

/// Remove a leading process-manager / service tag so pretty-block detection works.
///
/// Prefer an exact `[service]` match when the ingest service name is set. Otherwise
/// (or if that fails) strip a single leading `[token]` tag such as concurrently's
/// `[api] {` — common in shell-attach where the service defaults to a cwd path.
pub fn strip_service_prefix(line: &str, service: &str) -> String {
    let trimmed_start = line.trim_start();

    if !service.is_empty() {
        let prefix = format!("[{service}]");
        if let Some(rest) = trimmed_start.strip_prefix(&prefix) {
            return rest.trim_start().to_string();
        }
    }

    if let Some(rest) = strip_process_manager_tag(trimmed_start) {
        return rest.trim_start().to_string();
    }

    line.to_string()
}

/// Strip one leading `[token]` tag (non-empty, no spaces, ≤ 64 chars).
///
/// Preserves NestJS ConsoleLogger's `[Nest]` marker so format packs can match it.
fn strip_process_manager_tag(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('[')?;
    let end = rest.find(']')?;
    let tag = &rest[..end];
    if tag.is_empty() || tag.contains(' ') || tag.len() > 64 {
        return None;
    }
    if tag.eq_ignore_ascii_case("Nest") {
        return None;
    }
    Some(&rest[end + 1..])
}

/// Cheap gate: only object-shaped candidates enter recovery.
pub fn looks_like_object(input: &str) -> bool {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with('{') {
        return true;
    }
    if trimmed.starts_with("<ref *") {
        return true;
    }
    if trimmed.starts_with("Object:") || trimmed.starts_with("object:") {
        return true;
    }
    // `Foo {`, `Map(2) {`, `Set(1) {` — ident then optional `(…)` then `{`
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    if !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' || bytes[i] == b'$') {
        return false;
    }
    i += 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
    {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'(' {
        i += 1;
        let mut depth = 1i32;
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
    }
    i < bytes.len() && bytes[i] == b'{'
}

/// True when the cleaned line should start multiline pretty buffering.
///
/// Accepts bare `{`, incomplete `{ type: '…',`, and inspect wrappers that still
/// leave brace depth open after the line.
pub fn is_pretty_block_start(cleaned: &str) -> bool {
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed == "{" {
        return true;
    }
    // Complete single-line objects are handled elsewhere.
    if trimmed.starts_with('{') && trimmed.ends_with('}') && trimmed.len() > 1 {
        return false;
    }
    if !looks_like_object(trimmed) {
        return false;
    }
    // Incomplete object / wrapper still open after this line.
    brace_depth_delta(trimmed) > 0
}

/// Recover a JSON object from messy Nest / util.inspect / almost-JSON text.
///
/// Hot path: strict parse first. Repair scanner runs only when gated.
pub fn recover_json_object(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Pass 1 — strict JSON object
    if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed) {
        return Some(Value::Object(obj));
    }

    if !looks_like_object(trimmed) {
        return None;
    }

    // Extract outermost `{…}` when wrapped in Nest noise, then fused repair.
    let candidate = extract_outer_object(trimmed).unwrap_or(trimmed);

    // Pass 2 — one fused rewrite + parse
    if let Some(jsonish) = fused_repair_to_json(candidate) {
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&jsonish) {
            return Some(Value::Object(obj));
        }
        // Pass 3 — bounded salvage on rewritten text
        if let Some(obj) = salvage_object(&jsonish) {
            return Some(obj);
        }
    }

    // Salvage on the raw candidate (truncated dumps that never rewrote cleanly)
    salvage_object(candidate)
}

/// Try to parse a completed pretty block into a JSON object (recovery entrypoint).
#[cfg(test)]
pub fn parse_pretty_block(joined: &str) -> Option<Value> {
    recover_json_object(joined)
}

/// Single joined `_raw` payload (never explode a pretty block into N rows).
pub fn joined_raw_payload(joined: String) -> Value {
    Value::Object(Map::from_iter([("_raw".to_string(), Value::String(joined))]))
}

/// Convert a JS-object-literal dump into JSON text (fused repair without gating).
///
/// Handles: unquoted keys, single-quoted strings, `undefined` → `null`, trailing commas,
/// util.inspect stubs, class/Map wrappers, bare ISO/UUID values.
#[cfg(test)]
pub fn js_literal_to_json(input: &str) -> Option<String> {
    let candidate = extract_outer_object(input.trim()).unwrap_or(input.trim());
    fused_repair_to_json(candidate)
}

fn fused_repair_to_json(input: &str) -> Option<String> {
    #[cfg(test)]
    repair_stats::bump();

    let mut out = String::with_capacity(input.len() + 16);
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut in_double = false;
    let mut in_single = false;
    let mut escape = false;

    // Skip leading inspect wrappers before the first `{`
    i = skip_leading_wrappers(&chars, i);

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

        // Inspect bracket stubs: [Circular *N], [Object], [Array], [Function: …]
        if c == '[' {
            if let Some((end, replacement)) = match_inspect_stub(&chars, i) {
                out.push_str(replacement);
                i = end;
                continue;
            }
        }

        // Nested `<ref *N>` before a value — skip the marker (wrapper handled below).
        if c == '<' && matches_prefix(&chars, i, "<ref *") {
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            continue;
        }

        // `=>` Map arrow — stringify the following value span as a JSON string
        if c == '=' && i + 1 < chars.len() && chars[i + 1] == '>' {
            i += 2;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            let (next_i, quoted) = stringify_value_span(&chars, i);
            out.push(':');
            out.push(' ');
            out.push_str(&quoted);
            i = next_i;
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
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
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
            'u' if matches_keyword(&chars, i, "undefined") => {
                out.push_str("null");
                i += "undefined".len();
            }
            // Class / Map / Set wrapper immediately before `{` or `[` — skip the name
            c if is_ident_start(c) => {
                if let Some(after_wrapper) = skip_constructor_wrapper(&chars, i) {
                    i = after_wrapper;
                    continue;
                }
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
                    out.push('"');
                    out.push_str(&ident);
                    out.push('"');
                }
            }
            // Bare ISO-8601 / UUID-looking tokens in value position
            c if c.is_ascii_digit() => {
                if let Some((end, quoted)) = match_bare_iso_or_uuid(&chars, i) {
                    out.push('"');
                    out.push_str(&quoted);
                    out.push('"');
                    i = end;
                } else {
                    // Ordinary number — copy digits / sign / fraction / exponent
                    let start = i;
                    if chars[i] == '-' || chars[i] == '+' {
                        i += 1;
                    }
                    while i < chars.len()
                        && (chars[i].is_ascii_digit()
                            || chars[i] == '.'
                            || chars[i] == 'e'
                            || chars[i] == 'E'
                            || chars[i] == '+'
                            || chars[i] == '-')
                    {
                        // Stop if this looks like ISO date (digit-digit-digit-digit-)
                        if i > start && chars[i] == '-' && iso_lookahead(&chars, start) {
                            break;
                        }
                        i += 1;
                    }
                    // Re-check: if the full span is ISO/UUID, quote it
                    if let Some((end, quoted)) = match_bare_iso_or_uuid(&chars, start) {
                        out.push('"');
                        out.push_str(&quoted);
                        out.push('"');
                        i = end;
                    } else {
                        for ch in &chars[start..i] {
                            out.push(*ch);
                        }
                    }
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

fn skip_leading_wrappers(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        // `<ref *N>`
        if chars[i] == '<' && matches_prefix(chars, i, "<ref *") {
            while i < chars.len() && chars[i] != '>' {
                i += 1;
            }
            if i < chars.len() {
                i += 1; // '>'
            }
            continue;
        }
        // `Object:` label
        if matches_keyword(chars, i, "Object") || matches_keyword(chars, i, "object") {
            let after = i + "Object".len();
            let mut j = after;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && chars[j] == ':' {
                i = j + 1;
                continue;
            }
        }
        // Constructor wrapper before `{`
        if is_ident_start(chars[i]) {
            if let Some(after) = skip_constructor_wrapper(chars, i) {
                i = after;
                continue;
            }
        }
        break;
    }
    i
}

/// If `Ident` or `Ident(…)` is immediately followed by `{`/`[`, return index of that brace.
fn skip_constructor_wrapper(chars: &[char], i: usize) -> Option<usize> {
    if i >= chars.len() || !is_ident_start(chars[i]) {
        return None;
    }
    let mut j = i + 1;
    while j < chars.len() && is_ident_continue(chars[j]) {
        j += 1;
    }
    let mut k = j;
    while k < chars.len() && chars[k].is_whitespace() {
        k += 1;
    }
    if k < chars.len() && chars[k] == '(' {
        k += 1;
        let mut depth = 1i32;
        while k < chars.len() && depth > 0 {
            match chars[k] {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            k += 1;
        }
        while k < chars.len() && chars[k].is_whitespace() {
            k += 1;
        }
    }
    if k < chars.len() && (chars[k] == '{' || chars[k] == '[') {
        Some(k)
    } else {
        None
    }
}

fn match_inspect_stub(chars: &[char], i: usize) -> Option<(usize, &'static str)> {
    if i >= chars.len() || chars[i] != '[' {
        return None;
    }
    let rest: String = chars[i..].iter().take(64).collect();
    let replacements = [
        ("[Circular", "null"),
        ("[Object]", "null"),
        ("[Array]", "null"),
        ("[Function", "null"),
    ];
    for (prefix, replacement) in replacements {
        if rest.starts_with(prefix) {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != ']' {
                j += 1;
            }
            if j < chars.len() {
                return Some((j + 1, replacement));
            }
            return None;
        }
    }
    None
}

fn stringify_value_span(chars: &[char], i: usize) -> (usize, String) {
    if i >= chars.len() {
        return (i, "null".into());
    }
    // If value is already a structured `{` / `[` / quote, copy until balanced / closed
    let c = chars[i];
    if c == '{' || c == '[' {
        let open = c;
        let close = if c == '{' { '}' } else { ']' };
        let mut j = i + 1;
        let mut depth = 1i32;
        let mut in_d = false;
        let mut in_s = false;
        let mut esc = false;
        while j < chars.len() && depth > 0 {
            let ch = chars[j];
            if in_d {
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == '"' {
                    in_d = false;
                }
                j += 1;
                continue;
            }
            if in_s {
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == '\'' {
                    in_s = false;
                }
                j += 1;
                continue;
            }
            match ch {
                '"' => in_d = true,
                '\'' => in_s = true,
                ch if ch == open => depth += 1,
                ch if ch == close => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        let span: String = chars[i..j].iter().collect();
        let escaped = span.replace('\\', "\\\\").replace('"', "\\\"");
        return (j, format!("\"{escaped}\""));
    }
    if c == '\'' || c == '"' {
        let quote = c;
        let mut j = i + 1;
        let mut esc = false;
        while j < chars.len() {
            let ch = chars[j];
            if esc {
                esc = false;
                j += 1;
                continue;
            }
            if ch == '\\' {
                esc = true;
                j += 1;
                continue;
            }
            if ch == quote {
                j += 1;
                break;
            }
            j += 1;
        }
        let span: String = chars[i + 1..j.saturating_sub(1)].iter().collect();
        let escaped = span.replace('\\', "\\\\").replace('"', "\\\"");
        return (j, format!("\"{escaped}\""));
    }
    // Bare token until comma / brace
    let mut j = i;
    while j < chars.len()
        && chars[j] != ','
        && chars[j] != '}'
        && chars[j] != ']'
        && chars[j] != '\n'
    {
        j += 1;
    }
    let span: String = chars[i..j].iter().collect::<String>().trim().to_string();
    if span == "undefined" {
        return (j, "null".into());
    }
    let escaped = span.replace('\\', "\\\\").replace('"', "\\\"");
    (j, format!("\"{escaped}\""))
}

fn match_bare_iso_or_uuid(chars: &[char], i: usize) -> Option<(usize, String)> {
    if i >= chars.len() || !chars[i].is_ascii_digit() {
        return None;
    }
    // UUID: 8-4-4-4-12 hex
    if let Some(end) = match_uuid(chars, i) {
        let s: String = chars[i..end].iter().collect();
        return Some((end, s));
    }
    // ISO-8601-ish: YYYY-MM-DDTHH:MM:SS…Z?
    if i + 19 <= chars.len()
        && chars[i].is_ascii_digit()
        && chars[i + 1].is_ascii_digit()
        && chars[i + 2].is_ascii_digit()
        && chars[i + 3].is_ascii_digit()
        && chars[i + 4] == '-'
        && chars[i + 7] == '-'
        && (chars[i + 10] == 'T' || chars[i + 10] == 't')
    {
        let mut j = i + 11;
        while j < chars.len() {
            let ch = chars[j];
            if ch.is_ascii_alphanumeric() || ch == ':' || ch == '.' || ch == '+' || ch == '-' {
                j += 1;
            } else if ch == 'Z' || ch == 'z' {
                j += 1;
                break;
            } else {
                break;
            }
        }
        // Must consume past time portion
        if j >= i + 19 {
            let s: String = chars[i..j].iter().collect();
            return Some((j, s));
        }
    }
    None
}

fn iso_lookahead(chars: &[char], start: usize) -> bool {
    start + 10 <= chars.len()
        && chars[start].is_ascii_digit()
        && chars.get(start + 4) == Some(&'-')
        && chars.get(start + 7) == Some(&'-')
}

fn match_uuid(chars: &[char], i: usize) -> Option<usize> {
    // 8-4-4-4-12
    const LEN: usize = 36;
    if i + LEN > chars.len() {
        return None;
    }
    let s: String = chars[i..i + LEN].iter().collect();
    let b = s.as_bytes();
    if b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
        return None;
    }
    for (idx, ch) in b.iter().enumerate() {
        if idx == 8 || idx == 13 || idx == 18 || idx == 23 {
            continue;
        }
        if !ch.is_ascii_hexdigit() {
            return None;
        }
    }
    // Not part of a longer ident
    let end = i + LEN;
    if end < chars.len() && is_ident_continue(chars[end]) {
        return None;
    }
    Some(end)
}

fn matches_prefix(chars: &[char], i: usize, prefix: &str) -> bool {
    let p: Vec<char> = prefix.chars().collect();
    if i + p.len() > chars.len() {
        return false;
    }
    chars[i..i + p.len()] == p[..]
}

/// Extract the outermost balanced `{…}` span, if present.
fn extract_outer_object(input: &str) -> Option<&str> {
    let bytes = input.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_double = false;
    let mut in_single = false;
    let mut escape = false;
    // Walk as chars for quote awareness but slice by byte indices carefully —
    // we only look for ASCII braces/quotes here; object dumps are ASCII-heavy.
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut start_idx = None;
    for &(byte_i, c) in &chars {
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
            '{' => {
                if depth == 0 {
                    start_idx = Some(byte_i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let s = start_idx.unwrap_or(start);
                    let end = byte_i + c.len_utf8();
                    return Some(&input[s..end]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Bounded salvage: try prefixes ending at balanced `}` (≤ MAX_SALVAGE_ATTEMPTS parses).
fn salvage_object(text: &str) -> Option<Value> {
    let close_positions: Vec<usize> = {
        let mut positions = Vec::new();
        let mut depth = 0i32;
        let mut in_double = false;
        let mut in_single = false;
        let mut escape = false;
        for (byte_i, c) in text.char_indices() {
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
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        positions.push(byte_i + c.len_utf8());
                    }
                }
                _ => {}
            }
        }
        positions
    };

    if close_positions.is_empty() {
        return None;
    }

    // Try from the end: full object first, then earlier balanced closes.
    for &end in close_positions.iter().rev().take(MAX_SALVAGE_ATTEMPTS) {
        let prefix = &text[..end];
        // Prefer already-rewritten JSON; also try fused repair on the prefix.
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(prefix) {
            return Some(Value::Object(obj));
        }
        if let Some(jsonish) = fused_repair_to_json(prefix) {
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&jsonish) {
                return Some(Value::Object(obj));
            }
        }
    }
    None
}

fn matches_keyword(chars: &[char], i: usize, word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    if i + w.len() > chars.len() {
        return false;
    }
    if chars[i..i + w.len()] != w[..] {
        return false;
    }
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
pub fn brace_depth_delta(line: &str) -> i32 {
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
    fn strip_ansi_preserves_utf8() {
        assert_eq!(strip_ansi("failed — ok"), "failed — ok");
        assert_eq!(strip_ansi("\x1b[31m—\x1b[0m"), "—");
        assert_eq!(strip_ansi("café 日本語 🎉"), "café 日本語 🎉");
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
    fn strip_service_prefix_strips_process_manager_tags() {
        assert_eq!(strip_service_prefix("[other] {", "api"), "{");
        assert_eq!(
            strip_service_prefix("[api] {", "/Users/lucas/Documents/GitHub/monorepo"),
            "{"
        );
        assert_eq!(strip_service_prefix("{", "api"), "{");
        assert_eq!(strip_service_prefix("[my app] {", "api"), "[my app] {");
        assert_eq!(
            strip_service_prefix("[Nest] 1  - 15/08/2024, 23:30:49     LOG [App] hi", "api"),
            "[Nest] 1  - 15/08/2024, 23:30:49     LOG [App] hi"
        );
        assert_eq!(
            strip_service_prefix("[api] [Nest] 1  - 15/08/2024, 23:30:49     LOG hi", "api"),
            "[Nest] 1  - 15/08/2024, 23:30:49     LOG hi"
        );
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

    #[test]
    fn strip_ansi_osc_and_truncated() {
        assert_eq!(strip_ansi("\x1b]0;title\x07{"), "{");
        assert_eq!(strip_ansi("\x1b]0;title\x1b\\{"), "{");
        assert_eq!(strip_ansi("\x1b"), "");
        assert_eq!(strip_ansi("\x1bX{"), "X{");
    }

    #[test]
    fn js_literal_comments_trailing_commas_and_bare_values() {
        let jsonish = js_literal_to_json("{ // comment\n a: 1, b: undefined, c: bare, }").unwrap();
        assert!(jsonish.contains("\"a\": 1"));
        assert!(jsonish.contains("\"b\": null"));
        assert!(jsonish.contains("\"c\": \"bare\""));
        assert!(!jsonish.contains(",}"));
    }

    #[test]
    fn single_quote_escape_sequences() {
        let input = "{ s: 'it\\'s ok' }";
        let jsonish = js_literal_to_json(input).unwrap();
        let v: serde_json::Value = serde_json::from_str(&jsonish).unwrap();
        assert_eq!(v["s"], "it's ok");
    }

    #[test]
    fn buffer_helpers_and_invalid_block() {
        let buf = PrettyBuffer::start("{".into());
        assert_eq!(buf.into_lines(), vec!["{"]);
        let mut big = PrettyBuffer::start("{".into());
        for _ in 0..MAX_BUFFER_LINES {
            big.push("  x: 1,".into());
        }
        assert!(big.is_oversized());
        assert!(is_pretty_block_start("  {  "));
        assert!(parse_pretty_block("[1,2]").is_none());
        assert!(js_literal_to_json("{ bad: 'unclosed").is_none());
    }

    #[test]
    fn strip_service_prefix_exact_only_when_matching() {
        assert_eq!(strip_service_prefix("[api] {", "web"), "{");
        assert_eq!(strip_service_prefix("no prefix", ""), "no prefix");
        assert_eq!(strip_service_prefix("  [svc] x", "svc"), "x");
    }

    #[test]
    fn js_literal_newline_tab_carriage_escape() {
        let jsonish = js_literal_to_json("{ s: 'a\\nb\\tc' }").unwrap();
        assert!(jsonish.contains('a') && jsonish.contains('b') && jsonish.contains('c'));
    }

    #[test]
    fn double_quoted_escape_in_js_literal() {
        let jsonish = js_literal_to_json(r#"{ s: "a \"b\"" }"#).unwrap();
        let v: serde_json::Value = serde_json::from_str(&jsonish).unwrap();
        assert_eq!(v["s"], r#"a "b""#);
    }

    #[test]
    fn single_quoted_unknown_escape_and_bare_undefined() {
        let jsonish = js_literal_to_json("{ s: 'a\\z' }").unwrap();
        assert!(jsonish.contains('a'));
        let jsonish = js_literal_to_json("{ x: undefined, y: bare }").unwrap();
        let v: serde_json::Value = serde_json::from_str(&jsonish).unwrap();
        assert_eq!(v["x"], serde_json::Value::Null);
        assert_eq!(v["y"], "bare");
    }

    #[test]
    fn brace_depth_ignores_escaped_quotes() {
        let mut buf = PrettyBuffer::start("{".into());
        buf.push(r#"  key: 'a\'b',"#.into());
        buf.push("}".into());
        assert!(buf.is_complete());
    }

    #[test]
    fn recovers_inspect_ref_and_circular() {
        let block = r#"<ref *1> {
  type: 'inventory.relationships.evidence.decision',
  nested: <ref *1> {
    self: [Circular *1],
    stub: [Object],
  },
  level: 'info',
}"#;
        let value = recover_json_object(block).expect("should recover");
        assert_eq!(
            value["type"],
            json!("inventory.relationships.evidence.decision")
        );
        assert_eq!(value["nested"]["self"], json!(null));
        assert_eq!(value["nested"]["stub"], json!(null));
    }

    #[test]
    fn recovers_class_prefixed_object() {
        let block = "Foo {\n  a: 1,\n  b: 'x',\n}";
        let value = recover_json_object(block).expect("should recover");
        assert_eq!(value["a"], json!(1));
        assert_eq!(value["b"], json!("x"));
    }

    #[test]
    fn recovers_object_label_wrapper() {
        let block = "Object:\n{\n  level: 'warn',\n  msg: 'hi',\n}";
        let value = recover_json_object(block).expect("should recover");
        assert_eq!(value["level"], json!("warn"));
        assert_eq!(value["msg"], json!("hi"));
    }

    #[test]
    fn salvage_truncated_dump() {
        let block = r#"{
  type: 'inventory.relationships.evidence.decision',
  level: 'info',
  evidence: [
    { id: 1 },
    { id: 2, broken:
"#;
        let value = recover_json_object(block);
        // May salvage outer fields or fail to one path — must not panic
        if let Some(v) = value {
            assert!(v.get("type").is_some() || v.get("level").is_some());
        }
    }

    #[test]
    fn pretty_block_start_incomplete_opener() {
        assert!(is_pretty_block_start(
            "{ type: 'inventory.relationships.evidence.decision',"
        ));
        assert!(!is_pretty_block_start("{ level: 'info', msg: 'hi' }"));
        assert!(is_pretty_block_start("<ref *1> {"));
        assert!(is_pretty_block_start("Foo {"));
        assert!(!is_pretty_block_start("plain text"));
    }

    #[test]
    fn looks_like_object_gate() {
        assert!(looks_like_object("{ a: 1 }"));
        assert!(looks_like_object("  Map(2) {"));
        assert!(looks_like_object("Object:\n{"));
        assert!(!looks_like_object("hello world"));
        assert!(!looks_like_object("level=info msg=hi"));
    }

    #[test]
    fn strict_json_skips_repair_scanner() {
        reset_repair_scan_count();
        let v = recover_json_object(r#"{"level":"info","msg":"hi"}"#).unwrap();
        assert_eq!(v["msg"], json!("hi"));
        assert_eq!(repair_scan_count(), 0);
    }

    #[test]
    fn plain_text_skips_repair_scanner() {
        reset_repair_scan_count();
        assert!(recover_json_object("plain text line").is_none());
        assert_eq!(repair_scan_count(), 0);
    }

    #[test]
    fn large_evidence_decision_recovers() {
        let mut lines = vec![
            "{".into(),
            "  type: 'inventory.relationships.evidence.decision',".into(),
            "  level: 'info',".into(),
            "  evidence: [".into(),
        ];
        for i in 0..300 {
            lines.push(format!("    {{ id: {i}, ok: true }},"));
        }
        lines.push("  ],".into());
        lines.push("  message: 'done',".into());
        lines.push("}".into());
        let joined = lines.join("\n");
        assert!(joined.lines().count() > 256);
        let value = recover_json_object(&joined).expect("large dump should recover");
        assert_eq!(
            value["type"],
            json!("inventory.relationships.evidence.decision")
        );
        assert_eq!(value["message"], json!("done"));
        assert_eq!(value["evidence"].as_array().unwrap().len(), 300);
    }

    #[test]
    fn buffer_tracks_bytes_and_oversized_by_bytes() {
        let mut buf = PrettyBuffer::start("{".into());
        // Push a huge line to exceed byte cap without hitting line cap.
        let big = format!("  x: '{}',", "a".repeat(MAX_BUFFER_BYTES));
        buf.push(big);
        assert!(buf.is_oversized());
        assert!(buf.byte_len() >= MAX_BUFFER_BYTES);
    }

    #[test]
    fn bare_iso_timestamp_quoted() {
        let block = "{ ts: 2026-07-15T19:37:27.513Z, ok: true }";
        let value = recover_json_object(block).expect("should recover");
        assert_eq!(value["ts"], json!("2026-07-15T19:37:27.513Z"));
        assert_eq!(value["ok"], json!(true));
    }
}
