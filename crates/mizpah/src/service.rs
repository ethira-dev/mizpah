//! Smart default service name inference from the working directory.

use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_SERVICE: &str = "default";
const MAX_PARENT_WALK: usize = 8;

/// Resolve a service name: explicit override / env, else infer from `cwd`.
///
/// Precedence: CLI/`MIZPAH_SERVICE` (unsanitized) → `OTEL_SERVICE_NAME` →
/// `SERVICE_NAME` → [`infer_service_name`].
pub fn resolve_service(service: Option<&str>) -> String {
    if let Some(s) = service {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(name) = env_nonempty("MIZPAH_SERVICE") {
        return name;
    }
    if let Some(name) = env_nonempty("OTEL_SERVICE_NAME") {
        return sanitize_service_name(&name);
    }
    if let Some(name) = env_nonempty("SERVICE_NAME") {
        return sanitize_service_name(&name);
    }
    infer_service_name(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Infer a short service slug from a project directory.
///
/// Walks from `dir` toward the git root (or up to [`MAX_PARENT_WALK`] parents),
/// trying ecosystem manifests at each level. Then: git root basename →
/// directory basename → `"default"`.
pub fn infer_service_name(dir: &Path) -> String {
    let start = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let git_root = find_git_root(&start);

    let mut cur = start.clone();
    for depth in 0..=MAX_PARENT_WALK {
        if let Some(name) = infer_from_manifests(&cur) {
            return sanitize_service_name(&name);
        }
        if git_root.as_ref().is_some_and(|root| root == &cur) {
            break;
        }
        if depth == MAX_PARENT_WALK {
            break;
        }
        if !cur.pop() {
            break;
        }
    }

    if let Some(root) = git_root {
        if let Some(base) = root.file_name().and_then(|s| s.to_str()) {
            let slug = sanitize_service_name(base);
            if slug != DEFAULT_SERVICE {
                return slug;
            }
        }
    }
    if let Some(base) = start.file_name().and_then(|s| s.to_str()) {
        let slug = sanitize_service_name(base);
        if slug != DEFAULT_SERVICE {
            return slug;
        }
    }
    DEFAULT_SERVICE.to_string()
}

/// Lowercase slug: alphanumerics, `-`, `_`, `.` kept; other chars → `-`; trim `-`.
pub fn sanitize_service_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_SERVICE.to_string();
    }
    let candidate = if let Some(rest) = trimmed.strip_prefix('@') {
        rest.rsplit('/').next().unwrap_or(rest)
    } else if trimmed.contains('/') {
        trimmed.rsplit('/').next().unwrap_or(trimmed)
    } else {
        trimmed
    };

    let mut out = String::with_capacity(candidate.len());
    for ch in candidate.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || ch == '/' || ch == '\\' {
            if !out.ends_with('-') {
                out.push('-');
            }
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        DEFAULT_SERVICE.to_string()
    } else {
        out
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

fn infer_from_manifests(dir: &Path) -> Option<String> {
    read_package_json_name(dir)
        .or_else(|| read_deno_name(dir))
        .or_else(|| read_cargo_package_name(dir))
        .or_else(|| read_pyproject_name(dir))
        .or_else(|| read_setup_cfg_name(dir))
        .or_else(|| read_go_mod_name(dir))
        .or_else(|| read_composer_name(dir))
        .or_else(|| read_gemspec_name(dir))
        .or_else(|| read_pom_artifact_id(dir))
        .or_else(|| read_gradle_settings_name(dir))
        .or_else(|| read_csproj_name(dir))
        .or_else(|| read_pubspec_name(dir))
        .or_else(|| read_mix_app_name(dir))
        .or_else(|| read_julia_project_name(dir))
        .or_else(|| read_helm_chart_name(dir))
        .or_else(|| read_cmake_project_name(dir))
}

fn nonempty_string(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn json_name_field(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("name").and_then(|n| n.as_str()).and_then(nonempty_string)
}

fn read_package_json_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    json_name_field(&text)
}

fn read_deno_name(dir: &Path) -> Option<String> {
    for name in ["deno.json", "deno.jsonc"] {
        let path = dir.join(name);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let stripped = if name.ends_with('c') {
            strip_jsonc_comments(&text)
        } else {
            text
        };
        if let Some(n) = json_name_field(&stripped) {
            return Some(n);
        }
    }
    None
}

/// Strip `//` line and `/* */` block comments for deno.jsonc (naive but adequate).
fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b as char);
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
        }
        out.push(b as char);
        i += 1;
    }
    out
}

fn read_cargo_package_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .and_then(nonempty_string)
}

fn read_pyproject_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("pyproject.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("project")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .and_then(nonempty_string)
        .or_else(|| {
            value
                .get("tool")
                .and_then(|t| t.get("poetry"))
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .and_then(nonempty_string)
        })
}

fn read_setup_cfg_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("setup.cfg")).ok()?;
    let mut in_metadata = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_metadata = trimmed.eq_ignore_ascii_case("[metadata]");
            continue;
        }
        if !in_metadata {
            continue;
        }
        if let Some((key, val)) = trimmed.split_once('=') {
            if key.trim().eq_ignore_ascii_case("name") {
                return nonempty_string(val);
            }
        }
    }
    None
}

fn read_go_mod_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("go.mod")).ok()?;
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("module") else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() {
            continue;
        }
        let module = rest.split_whitespace().next()?;
        return go_module_service_name(module);
    }
    None
}

fn go_module_service_name(module: &str) -> Option<String> {
    let mut path = module.trim().trim_end_matches('/');
    if path.is_empty() {
        return None;
    }
    // Strip Go major-version suffix: …/foo/v2 → …/foo (not v0/v1).
    if let Some((prefix, ver)) = path.rsplit_once('/') {
        if ver.starts_with('v')
            && ver.len() > 1
            && ver[1..].chars().all(|c| c.is_ascii_digit())
            && ver != "v0"
            && ver != "v1"
        {
            path = prefix;
        }
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    nonempty_string(name)
}

fn read_composer_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("composer.json")).ok()?;
    json_name_field(&text)
}

fn read_gemspec_name(dir: &Path) -> Option<String> {
    let mut matches = Vec::new();
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("gemspec") {
            matches.push(path);
        }
    }
    if matches.len() != 1 {
        return None;
    }
    let text = fs::read_to_string(&matches[0]).ok()?;
    for line in text.lines() {
        let trimmed = line.trim();
        // spec.name = "foo" / .name = 'foo'
        if let Some(rest) = trimmed
            .strip_prefix("spec.name")
            .or_else(|| trimmed.strip_prefix(".name"))
        {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim();
                if let Some(name) = extract_quoted(rest) {
                    return nonempty_string(&name);
                }
            }
        }
    }
    None
}

fn extract_quoted(s: &str) -> Option<String> {
    let s = s.trim();
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &s[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn read_pom_artifact_id(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("pom.xml")).ok()?;
    pom_project_artifact_id(&text)
}

/// Project `<artifactId>` outside `<parent>`; ignore parent’s artifactId.
fn pom_project_artifact_id(text: &str) -> Option<String> {
    let mut depth_parent = 0i32;
    let mut i = 0;
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if lower[i..].starts_with("<parent") {
            let after = &lower[i + 7..];
            if after.starts_with('>') || after.starts_with(' ') || after.starts_with('\t') {
                depth_parent += 1;
                i += 7;
                continue;
            }
        }
        if depth_parent > 0 && lower[i..].starts_with("</parent>") {
            depth_parent -= 1;
            i += 9;
            continue;
        }
        if depth_parent == 0 && lower[i..].starts_with("<artifactid>") {
            let start = i + "<artifactId>".len();
            let rest = &text[start..];
            let end_rel = rest.to_ascii_lowercase().find("</artifactid>")?;
            let id = rest[..end_rel].trim();
            return nonempty_string(id);
        }
        i += 1;
    }
    None
}

fn read_gradle_settings_name(dir: &Path) -> Option<String> {
    for name in ["settings.gradle.kts", "settings.gradle"] {
        let path = dir.join(name);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(n) = gradle_root_project_name(&text) {
            return Some(n);
        }
    }
    None
}

fn gradle_root_project_name(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("rootProject.name") else {
            continue;
        };
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        if let Some(name) = extract_quoted(rest) {
            return nonempty_string(&name);
        }
        let token = rest.split_whitespace().next()?;
        let token = token.trim_matches(|c| c == '"' || c == '\'');
        return nonempty_string(token);
    }
    None
}

fn read_csproj_name(dir: &Path) -> Option<String> {
    let mut matches = Vec::new();
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("csproj") {
            matches.push(path);
        }
    }
    if matches.len() != 1 {
        return None;
    }
    let path = &matches[0];
    let text = fs::read_to_string(path).ok()?;
    if let Some(name) = xml_tag_text(&text, "AssemblyName") {
        return Some(name);
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(nonempty_string)
}

fn xml_tag_text(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let lower = text.to_ascii_lowercase();
    let open_l = open.to_ascii_lowercase();
    let close_l = close.to_ascii_lowercase();
    let start = lower.find(&open_l)? + open.len();
    let rest = &text[start..];
    let end = rest.to_ascii_lowercase().find(&close_l)?;
    nonempty_string(rest[..end].trim())
}

fn read_yaml_top_level_name(dir: &Path, filename: &str) -> Option<String> {
    let text = fs::read_to_string(dir.join(filename)).ok()?;
    for line in text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("name:") else {
            continue;
        };
        let rest = rest.trim();
        let name = rest.trim_matches(|c| c == '"' || c == '\'');
        return nonempty_string(name);
    }
    None
}

fn read_pubspec_name(dir: &Path) -> Option<String> {
    read_yaml_top_level_name(dir, "pubspec.yaml")
}

fn read_helm_chart_name(dir: &Path) -> Option<String> {
    read_yaml_top_level_name(dir, "Chart.yaml")
}

fn read_mix_app_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("mix.exs")).ok()?;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("@app") {
            let rest = rest.trim();
            if let Some(atom) = rest.strip_prefix(':') {
                let atom = atom
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or("");
                if let Some(n) = nonempty_string(atom) {
                    return Some(n);
                }
            }
        }
        if let Some(rest) = trimmed.strip_prefix("app:") {
            let rest = rest.trim();
            if let Some(atom) = rest.strip_prefix(':') {
                let atom = atom
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .next()
                    .unwrap_or("");
                if let Some(n) = nonempty_string(atom) {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn read_julia_project_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("Project.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("name")
        .and_then(|n| n.as_str())
        .and_then(nonempty_string)
}

fn read_cmake_project_name(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("CMakeLists.txt")).ok()?;
    let lower = text.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("project") {
        let abs = search_from + rel;
        let after = abs + "project".len();
        let rest = text[after..].trim_start();
        if !rest.starts_with('(') {
            search_from = after;
            continue;
        }
        let inner = rest[1..].trim_start();
        if inner.starts_with("${") {
            search_from = after;
            continue;
        }
        let name: String = inner
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
            .collect();
        if let Some(n) = nonempty_string(&name) {
            return Some(n);
        }
        search_from = after;
    }
    None
}

fn find_git_root(dir: &Path) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    loop {
        if cur.join(".git").exists() {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// Serialize env-mutating tests (process-global env).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_service_name("My App"), "my-app");
        assert_eq!(sanitize_service_name("@acme/api"), "api");
        assert_eq!(sanitize_service_name("  "), "default");
        assert_eq!(sanitize_service_name("foo_bar"), "foo_bar");
        assert_eq!(sanitize_service_name("vendor/pkg"), "pkg");
    }

    #[test]
    fn infer_from_package_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"@org/cool-api"}"#,
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "cool-api");
    }

    #[test]
    fn infer_from_deno_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("deno.jsonc"),
            "{\n  // comment\n  \"name\": \"deno-svc\"\n}\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "deno-svc");
    }

    #[test]
    fn infer_from_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"mizpah-demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "mizpah-demo");
    }

    #[test]
    fn cargo_workspace_only_skipped() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("ws-root");
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/foo\"]\n",
        )
        .unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(infer_service_name(&root), "ws-root");
    }

    #[test]
    fn infer_from_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"py-svc\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "py-svc");
    }

    #[test]
    fn infer_from_poetry_pyproject() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"poetry-svc\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "poetry-svc");
    }

    #[test]
    fn infer_from_setup_cfg() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("setup.cfg"),
            "[metadata]\nname = setup-cfg-svc\nversion = 1.0\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "setup-cfg-svc");
    }

    #[test]
    fn infer_from_go_mod() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module github.com/acme/go-svc\n\ngo 1.22\n")
            .unwrap();
        assert_eq!(infer_service_name(dir.path()), "go-svc");
    }

    #[test]
    fn go_mod_strips_major_version_suffix() {
        assert_eq!(
            go_module_service_name("github.com/acme/foo/v2"),
            Some("foo".into())
        );
        assert_eq!(
            go_module_service_name("github.com/acme/foo/v12"),
            Some("foo".into())
        );
        assert_eq!(
            go_module_service_name("github.com/acme/foo"),
            Some("foo".into())
        );
        // v0/v1 are not Go major-version suffixes
        assert_eq!(
            go_module_service_name("github.com/acme/foo/v1"),
            Some("v1".into())
        );
    }

    #[test]
    fn infer_from_composer() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("composer.json"),
            r#"{"name":"acme/php-api"}"#,
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "php-api");
    }

    #[test]
    fn infer_from_gemspec() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("mygem.gemspec"),
            "Gem::Specification.new do |spec|\n  spec.name = \"ruby-svc\"\nend\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "ruby-svc");
    }

    #[test]
    fn multiple_gemspecs_skipped() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("multi-gem");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("a.gemspec"), "spec.name = \"a\"\n").unwrap();
        fs::write(dir.join("b.gemspec"), "spec.name = \"b\"\n").unwrap();
        assert_eq!(infer_service_name(&dir), "multi-gem");
    }

    #[test]
    fn pom_ignores_parent_artifact_id() {
        let xml = r#"
<project>
  <parent>
    <groupId>org.springframework.boot</groupId>
    <artifactId>spring-boot-starter-parent</artifactId>
    <version>3.2.0</version>
  </parent>
  <artifactId>java-api</artifactId>
</project>
"#;
        assert_eq!(pom_project_artifact_id(xml), Some("java-api".into()));
    }

    #[test]
    fn infer_from_pom() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pom.xml"),
            r#"
<project>
  <parent>
    <artifactId>spring-boot-starter-parent</artifactId>
  </parent>
  <artifactId>java-api</artifactId>
</project>
"#,
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "java-api");
    }

    #[test]
    fn infer_from_gradle_settings() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("settings.gradle.kts"),
            "rootProject.name = \"gradle-svc\"\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "gradle-svc");
    }

    #[test]
    fn infer_from_csproj() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("MyApp.csproj"),
            r#"<Project><PropertyGroup><AssemblyName>dotnet-svc</AssemblyName></PropertyGroup></Project>"#,
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "dotnet-svc");
    }

    #[test]
    fn csproj_falls_back_to_stem() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("WebApi.csproj"), "<Project></Project>\n").unwrap();
        assert_eq!(infer_service_name(dir.path()), "webapi");
    }

    #[test]
    fn multiple_csproj_skipped() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("multi-cs");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("A.csproj"), "<Project></Project>\n").unwrap();
        fs::write(dir.join("B.csproj"), "<Project></Project>\n").unwrap();
        assert_eq!(infer_service_name(&dir), "multi-cs");
    }

    #[test]
    fn infer_from_pubspec() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("pubspec.yaml"),
            "name: flutter_svc\ndescription: demo\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "flutter_svc");
    }

    #[test]
    fn infer_from_mix() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("mix.exs"),
            "defmodule MyApp.MixProject do\n  @app :elixir_svc\nend\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "elixir_svc");
    }

    #[test]
    fn infer_from_julia_project() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Project.toml"),
            "name = \"JuliaSvc\"\nuuid = \"11111111-1111-1111-1111-111111111111\"\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "juliasvc");
    }

    #[test]
    fn infer_from_helm_chart() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Chart.yaml"),
            "apiVersion: v2\nname: helm-svc\nversion: 0.1.0\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "helm-svc");
    }

    #[test]
    fn infer_from_cmake() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.20)\nproject(CmakeSvc VERSION 1.0)\n",
        )
        .unwrap();
        assert_eq!(infer_service_name(dir.path()), "cmakesvc");
    }

    #[test]
    fn infer_from_dirname() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("my-service");
        fs::create_dir(&dir).unwrap();
        assert_eq!(infer_service_name(&dir), "my-service");
    }

    #[test]
    fn infer_git_root_basename() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("repo-name");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        let nested = root.join("crates").join("inner");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(infer_service_name(&nested), "repo-name");
    }

    #[test]
    fn nearest_parent_manifest_beats_git_basename() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("repo-name");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        let pkg = root.join("packages").join("api");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("package.json"), r#"{"name":"nearest-api"}"#).unwrap();
        let nested = pkg.join("src");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(infer_service_name(&nested), "nearest-api");
    }

    #[test]
    fn resolve_explicit_wins() {
        assert_eq!(resolve_service(Some("api")), "api");
        assert_eq!(resolve_service(Some("  api  ")), "api");
    }

    #[test]
    fn package_json_beats_dirname() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("wrong-name");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("package.json"), r#"{"name":"right-name"}"#).unwrap();
        assert_eq!(infer_service_name(&dir), "right-name");
    }

    #[test]
    fn env_mizpah_service_wins_unsanitized() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old_m = std::env::var_os("MIZPAH_SERVICE");
        let old_o = std::env::var_os("OTEL_SERVICE_NAME");
        let old_s = std::env::var_os("SERVICE_NAME");
        std::env::remove_var("OTEL_SERVICE_NAME");
        std::env::remove_var("SERVICE_NAME");
        std::env::set_var("MIZPAH_SERVICE", "My Custom");
        assert_eq!(resolve_service(None), "My Custom");
        restore_env("MIZPAH_SERVICE", old_m);
        restore_env("OTEL_SERVICE_NAME", old_o);
        restore_env("SERVICE_NAME", old_s);
    }

    #[test]
    fn env_otel_beats_service_name_and_is_sanitized() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old_m = std::env::var_os("MIZPAH_SERVICE");
        let old_o = std::env::var_os("OTEL_SERVICE_NAME");
        let old_s = std::env::var_os("SERVICE_NAME");
        std::env::remove_var("MIZPAH_SERVICE");
        std::env::set_var("OTEL_SERVICE_NAME", "Otel App");
        std::env::set_var("SERVICE_NAME", "other");
        assert_eq!(resolve_service(None), "otel-app");
        restore_env("MIZPAH_SERVICE", old_m);
        restore_env("OTEL_SERVICE_NAME", old_o);
        restore_env("SERVICE_NAME", old_s);
    }

    #[test]
    fn cli_beats_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old_m = std::env::var_os("MIZPAH_SERVICE");
        std::env::set_var("MIZPAH_SERVICE", "from-env");
        assert_eq!(resolve_service(Some("from-cli")), "from-cli");
        restore_env("MIZPAH_SERVICE", old_m);
    }

    fn restore_env(key: &str, old: Option<std::ffi::OsString>) {
        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
