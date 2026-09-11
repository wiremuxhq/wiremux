//! One-pass env substitution. Missing vars unset the field (not `""`).

use serde_json::Value;

enum Subst {
    Keep(String),
    Unset,
}

/// Walk every string. A missing `$VAR` / `${VAR}` / `{env:VAR}` unsets the field.
pub(crate) fn walk(value: Value) -> Value {
    match value {
        Value::String(s) => match subst_string(&s) {
            Subst::Keep(s) => Value::String(s),
            Subst::Unset => Value::Null,
        },
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(walk).filter(|v| !v.is_null()).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let v = walk(v);
                if !v.is_null() {
                    out.insert(k, v);
                }
            }
            Value::Object(out)
        }
        other => other,
    }
}

fn subst_string(input: &str) -> Subst {
    let mut out = String::new();
    let mut unset = false;
    let mut i = 0;
    while i < input.len() {
        if let Some((var, consumed)) = match_env_ref(&input[i..]) {
            match std::env::var(var) {
                Ok(v) => out.push_str(&v),
                Err(_) => unset = true,
            }
            i += consumed;
            continue;
        }
        let ch = input[i..].chars().next().expect("i in bounds");
        out.push(ch);
        i += ch.len_utf8();
    }
    if unset {
        Subst::Unset
    } else {
        Subst::Keep(out)
    }
}

fn match_env_ref(s: &str) -> Option<(&str, usize)> {
    if let Some(rest) = s.strip_prefix("{env:") {
        let end = rest.find('}')?;
        let var = &rest[..end];
        if is_ident(var) {
            return Some((var, "{env:".len() + end + 1));
        }
        return None;
    }
    if let Some(rest) = s.strip_prefix("${") {
        let end = rest.find('}')?;
        let var = &rest[..end];
        if is_ident(var) {
            return Some((var, 2 + end + 1));
        }
        return None;
    }
    if let Some(rest) = s.strip_prefix('$') {
        let len = ident_prefix_len(rest);
        if len > 0 {
            return Some((&rest[..len], 1 + len));
        }
    }
    None
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn ident_prefix_len(s: &str) -> usize {
    let mut chars = s.char_indices();
    match chars.next() {
        Some((_, c)) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return 0,
    }
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if c.is_ascii_alphanumeric() || c == '_' {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_var_unsets_string() {
        let v = walk(Value::String(
            "{env:WIREMUX_TEST_UNSET_VAR_9f3c2e1a}".into(),
        ));
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn no_var_keeps_text() {
        let v = walk(Value::String("https://example.invalid".into()));
        assert_eq!(v, Value::String("https://example.invalid".into()));
    }
}
