use cel::{Context, Program, Value as CelValue};
use serde_json::Value;
use std::sync::Arc;

/// A compiled CEL query, or match-all when the expression is empty.
#[derive(Clone)]
pub enum CompiledQuery {
    MatchAll,
    Cel(Arc<Program>),
}

/// Compile a CEL filter expression. Empty / whitespace → match all.
pub fn compile_query(q: &str) -> Result<CompiledQuery, String> {
    let trimmed = q.trim();
    if trimmed.is_empty() {
        return Ok(CompiledQuery::MatchAll);
    }
    Program::compile(trimmed)
        .map(|p| CompiledQuery::Cel(Arc::new(p)))
        .map_err(|e| format!("invalid CEL query: {e}"))
}

/// Evaluate a compiled query against a log entry.
///
/// Context bindings:
/// - `service` — stream service tag (wins over data key collisions)
/// - `level` — first of `level` / `severity` / `lvl` in data (wins over collisions)
/// - every top-level key from `data` as a CEL variable
///
/// The expression must evaluate to a bool. Execution errors and non-bool results
/// count as no match.
pub fn matches_entry(service: &str, data: &Value, query: &CompiledQuery) -> bool {
    match query {
        CompiledQuery::MatchAll => true,
        CompiledQuery::Cel(program) => {
            let Ok(ctx) = build_context(service, data) else {
                return false;
            };
            match program.execute(&ctx) {
                Ok(CelValue::Bool(b)) => b,
                _ => false,
            }
        }
    }
}

fn build_context(service: &str, data: &Value) -> Result<Context<'static>, String> {
    let mut ctx = Context::default();

    if let Value::Object(map) = data {
        for (key, value) in map {
            if key == "service" || key == "level" {
                continue;
            }
            ctx.add_variable(key.as_str(), value)
                .map_err(|e| format!("bind {key}: {e}"))?;
        }
    }

    ctx.add_variable("service", service)
        .map_err(|e| format!("bind service: {e}"))?;

    if let Some(level) = level_of(data) {
        ctx.add_variable("level", level)
            .map_err(|e| format!("bind level: {e}"))?;
    }

    Ok(ctx)
}

/// Mirror of UI `levelOf`: first of level / severity / lvl.
fn level_of(data: &Value) -> Option<String> {
    let Value::Object(map) = data else {
        return None;
    };
    for key in ["level", "severity", "lvl"] {
        if let Some(v) = map.get(key) {
            match v {
                Value::String(s) if !s.is_empty() => return Some(s.clone()),
                Value::Number(n) => return Some(n.to_string()),
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn matches(service: &str, data: &Value, q: &str) -> bool {
        let compiled = compile_query(q).expect("compile");
        matches_entry(service, data, &compiled)
    }

    #[test]
    fn empty_query_matches_all() {
        let data = json!({"msg": "hi"});
        assert!(matches("api", &data, ""));
        assert!(matches("api", &data, "   "));
    }

    #[test]
    fn eq_and_contains() {
        let data = json!({"msg": "hello world", "level": "info"});
        assert!(matches("api", &data, r#"level == "info""#));
        assert!(matches("api", &data, r#"msg.contains("world")"#));
        assert!(!matches("api", &data, r#"msg.contains("missing")"#));
    }

    #[test]
    fn and_or() {
        let data = json!({"msg": "timeout", "level": "error"});
        assert!(matches(
            "api",
            &data,
            r#"level == "error" && msg.contains("timeout")"#
        ));
        assert!(matches(
            "api",
            &data,
            r#"level == "info" || msg.contains("timeout")"#
        ));
        assert!(!matches(
            "api",
            &data,
            r#"level == "info" && msg.contains("timeout")"#
        ));
    }

    #[test]
    fn in_list() {
        let data = json!({"level": "error"});
        assert!(matches("api", &data, r#"level in ["warn", "error"]"#));
        assert!(!matches("api", &data, r#"level in ["info", "debug"]"#));
    }

    #[test]
    fn service_binding() {
        let data = json!({"msg": "hi"});
        assert!(matches("billing", &data, r#"service == "billing""#));
        assert!(!matches("billing", &data, r#"service == "api""#));
        assert!(matches("api", &data, r#"service in ["api", "web"]"#));
    }

    #[test]
    fn service_wins_over_data_key() {
        let data = json!({"service": "from-data", "msg": "hi"});
        assert!(matches("billing", &data, r#"service == "billing""#));
        assert!(!matches("billing", &data, r#"service == "from-data""#));
    }

    #[test]
    fn level_aliases() {
        let severity = json!({"severity": "warn"});
        assert!(matches("api", &severity, r#"level == "warn""#));

        let lvl = json!({"lvl": 50});
        assert!(matches("api", &lvl, r#"level == "50""#));

        let both = json!({"level": "error", "severity": "info"});
        assert!(matches("api", &both, r#"level == "error""#));
    }

    #[test]
    fn nested_path() {
        let data = json!({"user": {"id": "42"}, "level": "error"});
        assert!(matches("api", &data, r#"user.id == "42""#));
        assert!(matches("api", &data, r#"has(user.id)"#));
        assert!(!matches("api", &data, r#"user.id == "99""#));
    }

    #[test]
    fn missing_field_is_no_match() {
        let data = json!({"level": "info"});
        assert!(!matches("api", &data, r#"msg.contains("x")"#));
    }

    #[test]
    fn invalid_query_errors() {
        assert!(compile_query("level ==").is_err());
        assert!(compile_query("((((").is_err());
    }

    #[test]
    fn non_bool_is_no_match() {
        let data = json!({"msg": "hi", "n": 1});
        assert!(!matches("api", &data, r#"msg"#));
        assert!(!matches("api", &data, r#"1 + 1"#));
    }
}
