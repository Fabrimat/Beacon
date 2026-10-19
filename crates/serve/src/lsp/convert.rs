//! Byte offsets <-> LSP positions (UTF-16 columns), `file:` URIs <-> project paths, and
//! `diag::Diagnostic` -> LSP diagnostics.
//!
//! The engine speaks byte offsets (`Fix`, `ByteRange`) and 1-based line / byte-column spans;
//! LSP speaks 0-based lines and UTF-16 code-unit columns. Conversions clamp (an offset past
//! the end is the end, a column past the line end is the line end, a position inside a
//! surrogate pair rounds down to the character) and never panic on a stale line table.

use beacon_engine::diag::{Diagnostic, Severity, Span};
use beacon_engine::path::RelPath;
use lsp_types::{Position, Uri};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Byte offset of every line start (`\n` only; a `\r` stays at the end of its line).
pub fn line_starts(text: &str) -> Vec<u32> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i as u32 + 1))
        .collect()
}

/// `text[a..b]`, empty when the range is out of bounds or splits a character.
fn slice(text: &str, a: usize, b: usize) -> &str {
    text.get(a..b).unwrap_or("")
}

/// Largest char boundary `<= i`.
fn floor(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 0-based (line, UTF-16 column) of byte `offset`, using the file's line-start table.
pub fn position_at(text: &str, lines: &[u32], offset: u32) -> Position {
    let off = floor(text, offset as usize);
    let line = lines.partition_point(|&s| s as usize <= off).max(1) - 1;
    let start = lines.get(line).map_or(0, |&s| s as usize).min(off);
    Position {
        line: line as u32,
        character: slice(text, start, off).encode_utf16().count() as u32,
    }
}

/// Byte offset of an LSP position, using the file's line-start table (clamped to the line end
/// without its `\r\n`, or to the text end).
pub fn offset_at(text: &str, lines: &[u32], pos: Position) -> u32 {
    let Some(&start) = lines.get(pos.line as usize) else {
        return text.len() as u32;
    };
    let start = floor(text, start as usize);
    let end = lines
        .get(pos.line as usize + 1)
        .map_or(text.len(), |&e| e as usize - 1); // before the `\n`
    let line = slice(text, start, floor(text, end));
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (mut units, mut at) = (0u32, 0usize);
    for c in line.chars() {
        let n = c.len_utf16() as u32;
        if units + n > pos.character {
            break;
        }
        units += n;
        at += c.len_utf8();
    }
    (start + at) as u32
}

/// 0-based (line, UTF-16 column) of byte `offset` in `text` (clamped to the end).
pub fn position(text: &str, offset: u32) -> Position {
    position_at(text, &line_starts(text), offset)
}

/// Byte offset of an LSP position (clamped to the line end / text end).
pub fn offset(text: &str, pos: Position) -> u32 {
    offset_at(text, &line_starts(text), pos)
}

// ---------------------------------------------------------------- URIs

fn hex(b: u8) -> u8 {
    b"0123456789ABCDEF"[b as usize & 15]
}

/// `file:///D:/proj/a%20b.json` for an absolute path.
pub fn path_to_uri(p: &Path) -> Uri {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.strip_prefix("//?/").unwrap_or(&s);
    let mut out = String::from("file://");
    if !s.starts_with('/') {
        out.push('/');
    }
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.extend(['%', hex(b >> 4) as char, hex(b & 15) as char]);
        }
    }
    Uri::from_str(&out).expect("percent-encoded path is a valid URI")
}

fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The local path of a `file:` URI (`/d%3A/x` becomes `d:/x` on Windows).
pub fn uri_to_path(u: &Uri) -> Option<PathBuf> {
    let rest = u.as_str().strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let p = decode(rest)?;
    let drive = p.as_bytes().get(2) == Some(&b':') && p.as_bytes().first() == Some(&b'/');
    Some(PathBuf::from(if cfg!(windows) && drive {
        p[1..].to_owned()
    } else {
        p
    }))
}

/// The project-relative path of a `file:` URI under `root` (`None` outside it). Letter case
/// is ignored on Windows, where editors send `d:` for `D:`; a path spelled differently from
/// the canonical root (8.3 names, symlinks) is retried canonicalized.
pub fn rel_of(root: &Path, u: &Uri) -> Option<RelPath> {
    let path = uri_to_path(u)?;
    under(root, &path).or_else(|| under(root, &crate::discovery::canonical_root(&path)))
}

fn under(root: &Path, path: &Path) -> Option<RelPath> {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        s.strip_prefix("//?/").unwrap_or(&s).to_owned()
    };
    let (r, p) = (norm(root), norm(path));
    let fold = |s: &str| {
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    };
    let r = r.trim_end_matches('/');
    let head = p.get(..r.len())?;
    let rest = p[r.len()..].strip_prefix('/')?;
    (fold(head) == fold(r)).then_some(())?;
    RelPath::parse(rest).ok()
}

pub fn uri_of(root: &Path, rel: &RelPath) -> Uri {
    path_to_uri(&root.join(rel.as_str()))
}

// ---------------------------------------------------------------- diagnostics

/// An LSP range as JSON.
pub fn range(a: Position, b: Position) -> Value {
    json!({ "start": a, "end": b })
}

/// The LSP diagnostic of `d` located at `at` (text and line table of its file; `None` when
/// the file cannot be read: position 0:0). Spans carry a 1-based line and a byte column; the
/// range runs to the end of the identifier-like run that starts there (one character at
/// least).
pub fn diagnostic(
    d: &Diagnostic,
    text: Option<&str>,
    related: impl Fn(&Span) -> Option<Value>,
) -> Value {
    let (start, end) = match (d.span.as_ref(), text) {
        (
            Some(Span {
                line: Some(l),
                column,
                ..
            }),
            Some(t),
        ) => {
            let lines = line_starts(t);
            let ls = lines.get(*l as usize - 1).map_or(t.len(), |&s| s as usize);
            let a = floor(t, ls + column.unwrap_or(1) as usize - 1);
            let run = |c: char| c.is_alphanumeric() || "_:.-".contains(c);
            let len = t[a..].find(|c| !run(c)).unwrap_or(t.len() - a);
            let len = if len > 0 {
                len
            } else {
                t[a..].chars().next().map_or(0, char::len_utf8)
            };
            // never across the line end
            let eol = t[a..].find('\n').map_or(t.len(), |i| a + i);
            let b = (a + len).min(eol);
            (
                position_at(t, &lines, a as u32),
                position_at(t, &lines, b as u32),
            )
        }
        _ => (Position::default(), Position::default()),
    };
    let mut v = json!({
        "range": range(start, end),
        "severity": match d.severity {
            Severity::Error => 1,
            Severity::Warning => 2,
            Severity::Note => 3,
        },
        "code": d.code.id(),
        "source": "beacon",
        "message": d.message,
    });
    let rel: Vec<Value> = d.related.iter().filter_map(&related).collect();
    if !rel.is_empty() {
        v["relatedInformation"] = Value::Array(rel);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_round_trip() {
        // 'è' is 2 bytes / 1 unit, the emoji 4 bytes / 2 units
        let t = "a\r\nè=😀 demo:car\nlast";
        for (off, _) in t.char_indices() {
            let p = position(t, off as u32);
            if t.as_bytes()[off] == b'\n' {
                continue; // a column cannot name the position after `\r`
            }
            assert_eq!(offset(t, p) as usize, off, "{p:?}");
        }
        let id = t.find("demo").unwrap() as u32;
        assert_eq!(
            position(t, id),
            Position {
                line: 1,
                character: 5
            }
        );
        // clamping
        assert_eq!(
            offset(
                t,
                Position {
                    line: 9,
                    character: 0
                }
            ) as usize,
            t.len()
        );
        assert_eq!(
            offset(
                t,
                Position {
                    line: 0,
                    character: 99
                }
            ),
            1
        );
        // inside a surrogate pair: round down to the emoji start
        let emoji = t.find('😀').unwrap();
        assert_eq!(
            offset(
                t,
                Position {
                    line: 1,
                    character: 3
                }
            ) as usize,
            emoji
        );
        assert_eq!(position(t, 9999).line, 2);
    }

    #[test]
    fn uri_round_trip() {
        let root = if cfg!(windows) {
            Path::new("D:/p roj")
        } else {
            Path::new("/p roj")
        };
        let rel = RelPath::parse("src/é.json").unwrap();
        let u = uri_of(root, &rel);
        assert!(u.as_str().starts_with("file:///"), "{}", u.as_str());
        assert_eq!(rel_of(root, &u), Some(rel));
        if cfg!(windows) {
            let u = Uri::from_str("file:///d%3A/p%20roj/a.json").unwrap();
            assert_eq!(rel_of(root, &u), RelPath::parse("a.json").ok());
        }
        let other = Uri::from_str("file:///elsewhere/a.json").unwrap();
        assert_eq!(rel_of(root, &other), None);
    }
}
