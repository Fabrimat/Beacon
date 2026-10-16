//! Workstream A2: canonical JSON writer, `canonicalVersion` 1 (5.2): sorted keys, 2-space
//! indent, `\n` line endings, shortest round-trip numbers, minimal escaping, trailing newline.

use serde_json::Value;
use std::fmt::Write;

pub const CANONICAL_VERSION: u32 = 1;

pub fn to_vec(v: &Value) -> Vec<u8> {
    let mut s = String::new();
    write_value(&mut s, v, 0);
    s.push('\n');
    s.into_bytes()
}

fn indent(s: &mut String, depth: usize) {
    s.push('\n');
    s.extend(std::iter::repeat_n("  ", depth));
}

fn write_value(s: &mut String, v: &Value, depth: usize) {
    match v {
        Value::Null => s.push_str("null"),
        Value::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            let _ = write!(s, "{n}");
        }
        Value::String(x) => write_str(s, x),
        Value::Array(a) if a.is_empty() => s.push_str("[]"),
        Value::Array(a) => {
            s.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                indent(s, depth + 1);
                write_value(s, x, depth + 1);
            }
            indent(s, depth);
            s.push(']');
        }
        Value::Object(m) if m.is_empty() => s.push_str("{}"),
        Value::Object(m) => {
            // sorted here, not trusting serde_json's `preserve_order` feature state
            let mut entries: Vec<_> = m.iter().collect();
            entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            s.push('{');
            for (i, (k, x)) in entries.into_iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                indent(s, depth + 1);
                write_str(s, k);
                s.push_str(": ");
                write_value(s, x, depth + 1);
            }
            indent(s, depth);
            s.push('}');
        }
    }
}

fn write_str(s: &mut String, x: &str) {
    s.push('"');
    for c in x.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            '\u{8}' => s.push_str("\\b"),
            '\u{c}' => s.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(s, "\\u{:04x}", c as u32);
            }
            c => s.push(c),
        }
    }
    s.push('"');
}
