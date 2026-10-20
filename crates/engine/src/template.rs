//! `.mcfunction` and `.lang` templates ("subfunctions"): a fixed grammar, byte-compatible with
//! `regolith_subfunctions` 1.4.0 for the subset evaluated here, so migrated files are plain
//! copies. Data, not code: [`expand`] does no I/O and runs nothing, every limit is a
//! diagnostic, so `build`/`check` run it untrusted. The build stage ([`apply`]) reads only the
//! template and its module's `scope.json`, through the VFS.
//!
//! Grammar (blocks by indentation: the first child line fixes a block's indent):
//! - `UNPACK:SUBFUNCTION` (line 1 only): the file only defines subfunctions and emits nothing.
//! - `definefunction <n>:`, `[prefix ]function <n>:`, `[prefix ]schedule <args> <n>:`: the
//!   block becomes `X/n.mcfunction` beside `X.mcfunction`; the last two emit
//!   `{prefix}function X/n` / `{prefix}schedule {args} X/n` in its place.
//! - `for <v a..b [step]>:` (Python `range`: `b` is exclusive), `foreach <i v expr>:` (lists),
//!   `if <expr>:` (Python truthiness; 1.4.0 has no `else`/`elif`).
//! - `var n = expr`; `` `eval:expr` `` anywhere on a non-`#` line, block headers included
//!   (holes are filled before the header is matched).
//! - `#` lines are kept as is, `##` and blank lines dropped; output lines are stripped and
//!   joined by `\n` (no trailing newline). A file with no construct is left untouched.
//! - A plain command ending in `>:` and containing `<` (`say Press <E>:`) followed by an
//!   indented line is a malformed header (`TemplateSyntax`) and the file ships nothing; the
//!   same line without an indented child is an ordinary command.
//! - `.lang` (the destination decides, as the reference's `is_lang`): the function headers,
//!   `UNPACK:*` and `functiontree` are plain text, `##` lines are kept; the template writes only
//!   itself.
//!
//! Expressions: names, int/float/string literals, `f'..{expr}..'`, `x[i]`, `x['k']`, unary
//! `-`, `+ - *`, comparisons (one per operand pair, no chains), `in`/`not in`, `and`/`or`
//! (returning an operand, short-circuit), `not`, the methods `startswith`, `endswith`,
//! `capitalize`, `upper`, `lower` and the builtins `len`, `str`, `int`, parentheses; values
//! print like Python's `str()`. Everything else the reference accepts (`functiontree`,
//! `UNPACK:HERE`, `>`, `assert`, other calls, `/`, f-string format specs) is one coded
//! diagnostic, never silently skipped. Scope is lexical and file-local: `var` and loop
//! variables end with their block, `if` included (the reference leaks them through one shared
//! dict).

use crate::diag::{Code, Diagnostic};
use crate::merge::{lang_key, strip_comments};
use crate::model::{Body, ContentItem, Dest, Merge, Pack};
use crate::path::{Hash, RelPath, Vfs};
use serde_json::{Map, Number, Value};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

/// Template data of a module: a JSON(C) object beside `module.json`; absent = empty scope.
pub const SCOPE_FILE: &str = "scope.json";
/// Iterations of one `for`/`foreach`.
pub const MAX_LOOP: usize = 10_000;
/// Template lines evaluated, all loops included (empty nested loops cost no output).
pub const MAX_STEPS: usize = 1_000_000;
/// Output lines of one template, all its files together.
pub const MAX_LINES: usize = 100_000;
/// Files one template writes (itself and its subfunctions).
pub const MAX_FILES: usize = 1_000;
/// Block nesting.
pub const MAX_DEPTH: usize = 32;
/// Expression nesting (parentheses, subscripts, unary operators).
pub const MAX_EXPR_DEPTH: usize = 32;
/// Output bytes of one template.
pub const MAX_TEMPLATE_BYTES: usize = 16 << 20;
/// Longest string an expression may build (`+`).
pub const MAX_VALUE_BYTES: usize = 256 << 10;
/// Copying a scope/local value out of an expression, or building one, costs one step (of
/// [`MAX_STEPS`]) per this many bytes; reads and subscripts borrow and cost nothing.
pub const COPY_BYTES: usize = 256;
/// Visiting a source line costs one step plus one per this many bytes of it, so scanning
/// (trim, holes, matching, literals) is bounded to `MAX_STEPS * LINE_BYTES` (64 MiB).
pub const LINE_BYTES: usize = 64;
/// Output bytes of every template of a project together.
pub const MAX_PROJECT_BYTES: usize = 64 << 20;

/// One file a template writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    /// Pack-relative destination (`functions/m/x.mcfunction`, `functions/m/x/sub.mcfunction`).
    pub dest: RelPath,
    pub text: String,
    /// One entry per line of `text`, in order.
    pub line_map: Vec<LineOrigin>,
}

/// Where an output line came from (for symbol/LSP remapping and migrate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineOrigin {
    /// 1-based line in [`Expanded::text`].
    pub out_line: u32,
    /// 1-based line of the template source.
    pub src_line: u32,
    /// Byte range of the stripped command within that source line (indent excluded).
    pub src_cols: Range<u32>,
    /// The text is not a verbatim copy of `src_cols`: it went through an `eval` hole, or it
    /// is a rewritten header (`function <n>:` -> `function X/n`). Offsets do not map 1:1.
    pub computed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// The template's own file first (absent with `UNPACK:SUBFUNCTION`), then its
    /// subfunctions in the order they were defined.
    pub files: Vec<Expanded>,
    /// `false`: nothing to expand (the reference leaves such a file untouched): keep the
    /// source as is; `files` is empty.
    pub modified: bool,
    /// Top-level scope names the template read (not `var`/loop locals).
    pub scope_reads: BTreeSet<String>,
}

/// Template syntax in an `.mcfunction` (regolith_subfunctions would rewrite the file). A cheap
/// superset sniff; [`expand`] decides ([`Expansion::modified`]).
pub fn has_subfunctions(text: &str) -> bool {
    sniff(text, false)
}

/// [`has_subfunctions`] for a `.lang`: only block headers (`for`/`foreach`/`if`) and
/// `` `eval:` `` holes select a file. Narrower on purpose: the tree is parsed (and an odd
/// indent is an error) before [`expand`] knows whether anything expands, and an ordinary
/// `.lang` (`key=Press <E>:`, `var x=3`, `> note`) must stay untouched. A selected file is
/// expanded with the full grammar (`var` included).
pub fn has_lang_subfunctions(text: &str) -> bool {
    sniff(text, true)
}

fn sniff(text: &str, lang: bool) -> bool {
    const BOTH: &[&str] = &["for <", "foreach <", "if <"];
    // in a `.lang` these are plausible text (`var x=3`, `> note`): they do not select the file
    const MCFUNCTION: &[&str] = &[
        "var ",
        "> ",
        "assert ",
        "function <",
        "definefunction <",
        "UNPACK:",
    ];
    text.lines().any(|l| {
        let t = l.trim();
        let extra: &[&str] = if lang { &[] } else { MCFUNCTION };
        BOTH.iter().chain(extra).any(|p| t.starts_with(p))
            // every block header (`... run function <n>:`, `schedule .. <n>:`)
            || (!lang && t.ends_with(">:") && t.contains('<'))
            || (!t.starts_with('#') && l.contains("`eval:"))
    })
}

/// Expands one template. `source` only labels diagnostics (always the template's line and
/// column, never an output path); `base_dest` is where the template itself routes.
pub fn expand(
    text: &str,
    scope: &Value,
    source: &RelPath,
    base_dest: &RelPath,
) -> Result<Expansion, Diagnostic> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let nodes = parse(text, source)?;
    let empty = Map::new();
    let mut w = Walker {
        nodes: &nodes,
        source,
        env: Env {
            root: scope.as_object().unwrap_or(&empty),
            locals: Vec::new(),
            reads: BTreeSet::new(),
        },
        files: Vec::new(),
        dests: BTreeMap::from([(base_dest.clone(), 0)]),
        lang: base_dest.as_str().ends_with(".lang"),
        modified: false,
        unpacked: false,
        lines: 0,
        bytes: 0,
        steps: 0,
    };
    let lines = w.block(0, base_dest, Mode::Normal)?;
    if w.modified && !w.unpacked {
        w.files.insert(0, file(base_dest.clone(), lines));
    }
    Ok(Expansion {
        files: w.files,
        modified: w.modified,
        scope_reads: w.env.reads,
    })
}

/// Build stage, after routing and before static outputs join (prebuilt packs are copied as
/// is): every routed BP `.mcfunction` passthrough that is a template is replaced by its
/// expansions (`Body::Text`, origin = the template, `Merge::Error`, so a hand-written
/// `X/n.mcfunction` is a `RouteCollision`). A `.lang` template (any pack) keeps its item's
/// merge policy, so its text joins the other modules' lines (`Merge::LangLines`); a key it
/// defines twice is a `LangDuplicateKey` at the source line. A template that fails ships
/// nothing. Returns a hash of what changed, to fold into `Routed::key`; `None` when no
/// template did. `templates` gets every `.mcfunction` template source (even one that failed)
/// with where it routes (`.lang` templates are not listed there).
// ponytail: no cache, every build re-reads and re-expands (39 files, microseconds).
pub(crate) fn apply(
    items: &mut Vec<ContentItem>,
    templates: &mut BTreeMap<RelPath, Dest>,
    vfs: &dyn Vfs,
    diags: &mut Vec<Diagnostic>,
) -> Option<Hash> {
    let mut scopes: HashMap<RelPath, Value> = HashMap::new();
    let (mut h, mut any, mut total) = (blake3::Hasher::new(), false, 0usize);
    let mut out = Vec::with_capacity(items.len());
    for item in std::mem::take(items) {
        let lang = item.dest.path.as_str().ends_with(".lang");
        let src = match &item.body {
            Body::Passthrough(s)
                if lang
                    || (item.dest.pack == Pack::Behavior
                        && item.dest.path.as_str().ends_with(".mcfunction")) =>
            {
                s.clone()
            }
            _ => {
                out.push(item);
                continue;
            }
        };
        // A `.lang` is streamed: verified against the index but not cached, so a plain one stays
        // first read (and re-verified) by the lang merge, which retries if it changed mid-build.
        let read = match lang {
            true => {
                let mut b = Vec::new();
                vfs.stream(&src, &mut b).map(|_| b)
            }
            false => vfs.read(&src),
        };
        let Some(text) = read
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|t| match lang {
                true => has_lang_subfunctions(t),
                false => has_subfunctions(t),
            })
        else {
            out.push(item);
            continue;
        };
        if !scopes.contains_key(&item.module) {
            let s = load_scope(vfs, &item.module, diags);
            scopes.insert(item.module.clone(), s);
        }
        if !lang {
            templates.insert(src.clone(), item.dest.clone());
        }
        h.update(src.as_str().as_bytes());
        h.update(&[0]);
        let x = match expand(&text, &scopes[&item.module], &src, &item.dest.path) {
            Ok(x) if !x.modified => {
                out.push(item);
                continue;
            }
            Ok(x) => x,
            Err(d) => {
                diags.push(d);
                any = true;
                h.update(&[1]);
                continue;
            }
        };
        if lang && let Some(d) = x.files.iter().find_map(|f| duplicate_key(f, &src)) {
            diags.push(d);
            any = true;
            h.update(&[1]);
            continue;
        }
        any = true;
        let size: usize = x.files.iter().map(|f| f.text.len()).sum();
        if total + size > MAX_PROJECT_BYTES {
            diags.push(
                Diagnostic::new(
                    Code::TemplateLimit,
                    format!(
                        "templates of this project expand to more than {MAX_PROJECT_BYTES} bytes; {src} was not expanded"
                    ),
                )
                .at(src.clone(), None, None),
            );
            h.update(&[2]);
            continue;
        }
        total += size;
        for f in x.files {
            h.update(f.dest.as_str().as_bytes());
            h.update(&[0]);
            h.update(f.text.as_bytes());
            h.update(&[0]);
            out.push(ContentItem {
                dest: Dest {
                    path: f.dest,
                    ..item.dest.clone()
                },
                body: Body::Text(f.text),
                merge: if lang { item.merge } else { Merge::Error },
                origin: src.clone(),
                module: item.module.clone(),
            });
        }
    }
    if any {
        out.sort_by(|a, b| (&a.dest, &a.origin).cmp(&(&b.dest, &b.origin)));
    }
    *items = out;
    any.then(|| *h.finalize().as_bytes())
}

/// The first key `f` (a `.lang` expansion) defines twice, at its source line (the merge only
/// sees output lines, and only when other files share the destination).
pub(crate) fn duplicate_key(f: &Expanded, src: &RelPath) -> Option<Diagnostic> {
    let mut seen: HashMap<&str, u32> = HashMap::new();
    f.text.split('\n').zip(&f.line_map).find_map(|(l, o)| {
        let k = lang_key(l)?;
        let first = match seen.get(k) {
            Some(first) => *first,
            None => {
                seen.insert(k, o.src_line);
                return None;
            }
        };
        Some(
            Diagnostic::new(
                Code::LangDuplicateKey,
                format!("lang key {k:?} defined twice (first by line {first})"),
            )
            .at(src.clone(), Some(o.src_line), Some(o.src_cols.start + 1)),
        )
    })
}

/// The module's `scope.json` (BOM and JSONC accepted); missing = empty, invalid = a
/// diagnostic and empty.
pub(crate) fn load_scope(vfs: &dyn Vfs, module: &RelPath, diags: &mut Vec<Diagnostic>) -> Value {
    let Ok(path) = module.join(SCOPE_FILE) else {
        return Value::Null;
    };
    let Ok(bytes) = vfs.read(&path) else {
        return Value::Null;
    };
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    match serde_json::from_slice::<Value>(&strip_comments(bytes)) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => {
            diags.push(
                Diagnostic::new(
                    Code::TemplateSyntax,
                    format!("{path} must be a JSON object"),
                )
                .at(path, None, None),
            );
            Value::Null
        }
        Err(e) => {
            let (line, col) = (e.line() as u32, e.column() as u32);
            diags.push(
                Diagnostic::new(Code::TemplateSyntax, format!("{path}: {e}")).at(
                    path,
                    Some(line),
                    Some(col),
                ),
            );
            Value::Null
        }
    }
}

// ---------------------------------------------------------------------------------------
// tree

struct Node<'t> {
    /// 1-based; 0 = root.
    line: u32,
    /// Leading spaces (tabs are not indentation, as in the reference); root = -1.
    indent: i64,
    raw: &'t str,
    child_indent: Option<i64>,
    children: Vec<usize>,
}

/// The reference's `CodeTree`: node 0 is the root; blank lines are dropped.
fn parse<'t>(text: &'t str, source: &RelPath) -> Result<Vec<Node<'t>>, Diagnostic> {
    let mut nodes = vec![Node {
        line: 0,
        indent: -1,
        raw: "",
        child_indent: Some(0),
        children: Vec::new(),
    }];
    let mut stack = vec![0usize];
    // ponytail: `\n` and `\r\n` only; a lone `\r` (or `\x0b`, ` `..) is not a line break
    // here, unlike Python's `splitlines`.
    for (i, raw) in text.split('\n').enumerate() {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.trim().is_empty() {
            continue;
        }
        let line = i as u32 + 1;
        let indent = raw.bytes().take_while(|b| *b == b' ').count() as i64;
        let mut top = *stack.last().expect("root");
        if indent > nodes[top].indent {
            nodes[top].child_indent.get_or_insert(indent);
        } else {
            while nodes[top].child_indent.is_none_or(|c| indent < c) {
                stack.pop();
                top = *stack.last().expect("root has indent 0");
            }
        }
        let want = nodes[top].child_indent.expect("set above");
        if indent != want {
            return Err(Diagnostic::new(
                Code::TemplateSyntax,
                format!("indentation error: expected an indent of {want} spaces, found {indent}"),
            )
            .at(source.clone(), Some(line), Some(indent as u32 + 1)));
        }
        if stack.len() > MAX_DEPTH {
            return Err(Diagnostic::new(
                Code::TemplateLimit,
                format!("blocks nest deeper than {MAX_DEPTH} levels"),
            )
            .at(source.clone(), Some(line), Some(indent as u32 + 1)));
        }
        let n = nodes.len();
        nodes.push(Node {
            line,
            indent,
            raw,
            child_indent: None,
            children: Vec::new(),
        });
        nodes[top].children.push(n);
        stack.push(n);
    }
    Ok(nodes)
}

// ---------------------------------------------------------------------------------------
// statements

#[derive(Debug, PartialEq)]
enum Stmt<'l> {
    /// name, expression, byte offset of the expression.
    Var(&'l str, &'l str, usize),
    /// A construct of the reference that is not evaluated here.
    Unsupported(&'static str),
    Define(&'l str),
    Function(&'l str, &'l str),
    /// prefix, args, name.
    Schedule(&'l str, &'l str, &'l str),
    For(&'l str, i64, i64, i64),
    /// index, value, expression, byte offset of the expression.
    Foreach(&'l str, &'l str, &'l str, usize),
    /// condition, byte offset of the condition.
    If(&'l str, usize),
    Unpack,
    Command,
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `NAME(/NAME)*`
pub(crate) fn is_fn_name(s: &str) -> bool {
    s.split('/').all(is_name)
}

fn int(s: &str) -> Option<i64> {
    let d = s.strip_prefix('-').unwrap_or(s);
    (!d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// `a +b` -> (`a`, `b`): a split at the first space run (one space at least).
fn split_spaces(s: &str) -> Option<(&str, &str)> {
    let (a, b) = s.split_once(' ')?;
    Some((a, b.trim_start_matches(' ')))
}

/// The reference's matchers, in its order, on a stripped line with its holes filled. In a
/// `.lang` the function headers, `functiontree` and `UNPACK:*` are text.
fn classify(l: &str, lang: bool) -> Stmt<'_> {
    if let Some(r) = l.strip_prefix("var ") {
        let r = r.trim_start_matches(' ');
        let n = r
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(r.len());
        if let Some(e) = r[n..].trim_start_matches(' ').strip_prefix('=') {
            let e = e.trim_start_matches(' ');
            if n > 0 && !e.is_empty() && !e.contains('`') {
                return Stmt::Var(&r[..n], e, l.len() - e.len());
            }
        }
    }
    if l.starts_with("> ") {
        return Stmt::Unsupported(">");
    }
    let header = l.strip_suffix(">:");
    if let Some(n) = l
        .strip_prefix("definefunction <")
        .and_then(|r| r.strip_suffix(">:"))
        .filter(|n| is_fn_name(n) && !lang)
    {
        return Stmt::Define(n);
    }
    if let Some((head, lt)) = header
        .filter(|_| !lang)
        .and_then(|h| Some((h, h.rfind('<')?)))
    {
        let name = &head[lt + 1..];
        if is_fn_name(name) {
            let before = &head[..lt];
            if let Some(f) = before.strip_suffix("function ")
                && (f.is_empty() || f.ends_with(' '))
            {
                return Stmt::Function(f, name);
            }
            // `(.* )?schedule (.+) <name>:`, the prefix as long as possible
            if let Some(b) = before.strip_suffix(' ')
                && let Some(p) = b.rmatch_indices("schedule ").map(|(p, _)| p).find(|&p| {
                    (p == 0 || b.as_bytes()[p - 1] == b' ') && b.len() > p + "schedule ".len()
                })
            {
                return Stmt::Schedule(&b[..p], &b[p + "schedule ".len()..], name);
            }
        }
    }
    if let Some(e) = l
        .strip_prefix("if <")
        .and_then(|r| r.strip_suffix(">:"))
        .filter(|e| !e.is_empty() && !e.contains('`'))
    {
        return Stmt::If(e, "if <".len());
    }
    if let Some((v, rest)) = l
        .strip_prefix("for <")
        .and_then(|r| r.strip_suffix(">:"))
        .and_then(split_spaces)
        .filter(|(v, _)| is_name(v))
    {
        let (range, step) = match split_spaces(rest) {
            Some((r, s)) => (r, int(s)),
            None => (rest, Some(1)),
        };
        if let (Some((a, b)), Some(step)) = (range.split_once(".."), step)
            && let (Some(a), Some(b)) = (int(a), int(b))
        {
            return Stmt::For(v, a, b, step);
        }
    }
    if let Some((i, (v, e))) = l
        .strip_prefix("foreach <")
        .and_then(|r| r.strip_suffix(">:"))
        .and_then(split_spaces)
        .and_then(|(i, r)| Some((i, split_spaces(r)?)))
        && is_name(i)
        && is_name(v)
        && !e.is_empty()
        && !e.contains('`')
    {
        return Stmt::Foreach(i, v, e, l.len() - 2 - e.len());
    }
    if l.starts_with("functiontree <") && header.is_some() && !lang {
        return Stmt::Unsupported("functiontree");
    }
    if l.starts_with("assert ") {
        return Stmt::Unsupported("assert");
    }
    match l {
        "UNPACK:HERE" if !lang => Stmt::Unsupported("UNPACK:HERE"),
        "UNPACK:SUBFUNCTION" if !lang => Stmt::Unpack,
        _ => Stmt::Command,
    }
}

// ---------------------------------------------------------------------------------------
// walker

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    /// After `UNPACK:SUBFUNCTION`: only definitions (and loops around them).
    Unpack,
}

struct Line {
    text: String,
    src_line: u32,
    cols: Range<u32>,
    computed: bool,
}

struct Env<'a> {
    root: &'a Map<String, Value>,
    /// Innermost last; blocks truncate what they pushed.
    locals: Vec<(String, Value)>,
    reads: BTreeSet<String>,
}

struct Walker<'a> {
    nodes: &'a [Node<'a>],
    source: &'a RelPath,
    env: Env<'a>,
    files: Vec<Expanded>,
    /// Every file written so far -> line that defined it.
    dests: BTreeMap<RelPath, u32>,
    /// A `.lang` template (the reference's `is_lang`).
    lang: bool,
    modified: bool,
    unpacked: bool,
    lines: usize,
    bytes: usize,
    steps: usize,
}

fn file(dest: RelPath, lines: Vec<Line>) -> Expanded {
    let text = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let line_map = lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| LineOrigin {
            out_line: i as u32 + 1,
            src_line: l.src_line,
            src_cols: l.cols,
            computed: l.computed,
        })
        .collect();
    Expanded {
        dest,
        text,
        line_map,
    }
}

/// `s` cut to 80 chars for a message.
fn clip(s: &str) -> Cow<'_, str> {
    match s.char_indices().nth(80) {
        Some((i, _)) => Cow::Owned(format!("{}...", &s[..i])),
        None => Cow::Borrowed(s),
    }
}

/// Byte offset `off` of `s` as a char count.
fn chars(s: &str, off: usize) -> usize {
    s.get(..off)
        .map_or(s.chars().count(), |p| p.chars().count())
}

impl<'a> Walker<'a> {
    /// `col` = 0-based char column in the raw source line.
    fn diag(&self, code: Code, n: &Node, col: usize, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::new(code, msg).at(self.source.clone(), Some(n.line), Some(col as u32 + 1))
    }

    fn line(
        &mut self,
        text: String,
        n: &Node,
        cols: &Range<u32>,
        computed: bool,
    ) -> Result<Line, Diagnostic> {
        self.lines += 1;
        self.bytes += text.len() + 1;
        if self.lines > MAX_LINES || self.bytes > MAX_TEMPLATE_BYTES {
            let col = chars(n.raw, cols.start as usize);
            return Err(self.diag(
                Code::TemplateLimit,
                n,
                col,
                format!("template output exceeds {MAX_LINES} lines or {MAX_TEMPLATE_BYTES} bytes"),
            ));
        }
        Ok(Line {
            text,
            src_line: n.line,
            cols: cols.clone(),
            computed,
        })
    }

    /// Fills the `` `eval:expr` `` holes of a stripped line; `true` when there were any.
    fn holes(&mut self, s: &str, n: &Node, col0: usize) -> Result<(String, bool), Diagnostic> {
        let (mut out, mut cur, mut from) = (String::new(), 0, 0);
        while let Some(p) = s[from..].find("`eval:").map(|p| p + from) {
            let body = p + "`eval:".len();
            let Some(j) = s[body..].find('`').filter(|j| *j > 0) else {
                from = p + 1;
                continue;
            };
            let raw = &s[body..body + j];
            let pad = raw.len() - raw.trim_start_matches(' ').len();
            let v = self
                .eval(&raw[pad..], false)
                .and_then(|v| {
                    py_str(&v).map_err(|m| ExprErr {
                        at: 0,
                        code: Code::TemplateExpr,
                        msg: m,
                    })
                })
                .and_then(|v| match v.contains(['\n', '\r']) {
                    true => Err(ExprErr {
                        at: 0,
                        code: Code::TemplateExpr,
                        msg: "the value contains a line break".into(),
                    }),
                    false => Ok(v),
                })
                .map_err(|e| {
                    let col = col0 + chars(s, body + pad + e.at);
                    self.diag(
                        e.code,
                        n,
                        col,
                        format!("`eval:{}`: {}", clip(raw.trim()), e.msg),
                    )
                })?;
            out.push_str(&s[cur..p]);
            out.push_str(&v);
            cur = body + j + 1;
            from = cur;
        }
        if cur == 0 {
            return Ok((s.to_owned(), false));
        }
        out.push_str(&s[cur..]);
        Ok((out, true))
    }

    /// Evaluates `e`; copying a scope or local value out costs steps by size ([`COPY_BYTES`]).
    /// `truth`: only its truthiness is wanted (`Value::Bool`, nothing copied).
    fn eval(&mut self, e: &str, truth: bool) -> Result<Value, ExprErr> {
        let Env {
            root,
            locals,
            reads,
        } = &mut self.env;
        let mut p = Parser {
            s: e,
            i: 0,
            depth: 0,
            dead: false,
            root,
            locals,
            reads,
            steps: &mut self.steps,
        };
        let v = p.test()?;
        p.ws();
        if p.i < e.len() {
            return Err(p.unsupported());
        }
        match v {
            _ if truth => Ok(Value::Bool(truthy(&v))),
            Cow::Borrowed(v) => {
                p.charge(weight(v))?;
                Ok(v.clone())
            }
            Cow::Owned(v) => Ok(v),
        }
    }

    /// `cost` units of work against [`MAX_STEPS`]: a loop iteration is 1, a visited line 1 plus
    /// its length / [`LINE_BYTES`] (scanning a long line is work even when it emits nothing).
    fn step(&mut self, n: &Node, col: usize, cost: usize) -> Result<(), Diagnostic> {
        self.steps += cost;
        match self.steps > MAX_STEPS {
            true => Err(self.diag(
                Code::TemplateLimit,
                n,
                col,
                format!(
                    "template does more than {MAX_STEPS} steps (lines, loop iterations, copies of large values)"
                ),
            )),
            false => Ok(()),
        }
    }

    /// `X/name.mcfunction` beside `dest` (= `X.mcfunction`) and its function name, unique
    /// within the template.
    fn sub_dest(
        &mut self,
        dest: &RelPath,
        name: &str,
        n: &Node,
        col: usize,
    ) -> Result<(RelPath, String), Diagnostic> {
        let stem = dest
            .as_str()
            .strip_suffix(".mcfunction")
            .unwrap_or(dest.as_str());
        let d = RelPath::parse(&format!("{stem}/{name}.mcfunction")).map_err(|e| {
            self.diag(
                Code::TemplateSyntax,
                n,
                col,
                format!("subfunction name {name:?}: {}", e.0),
            )
        })?;
        if let Some(first) = self.dests.insert(d.clone(), n.line) {
            return Err(self.diag(
                Code::TemplateSyntax,
                n,
                col,
                format!("subfunction {d} is defined twice (first by line {first})"),
            ));
        }
        if self.dests.len() > MAX_FILES {
            return Err(self.diag(
                Code::TemplateLimit,
                n,
                col,
                format!("template writes more than {MAX_FILES} files"),
            ));
        }
        let call = stem.strip_prefix("functions/").unwrap_or(stem);
        Ok((d, format!("{call}/{name}")))
    }

    /// Evaluates the children of node `parent` (written to `dest`) into output lines.
    fn block(
        &mut self,
        parent: usize,
        dest: &RelPath,
        mut mode: Mode,
    ) -> Result<Vec<Line>, Diagnostic> {
        let nodes = self.nodes;
        let mark = self.env.locals.len();
        let mut out = Vec::new();
        for &c in &nodes[parent].children {
            let n = &nodes[c];
            // before anything scans the line (even trimming a long indent)
            self.step(n, 0, 1 + n.raw.len() / LINE_BYTES)?;
            let s = n.raw.trim();
            if s.starts_with("##") && !self.lang {
                continue;
            }
            let lead = n.raw.len() - n.raw.trim_start().len();
            let col0 = chars(n.raw, lead);
            let cols = lead as u32..(lead + s.len()) as u32;
            let comment = s.starts_with('#');
            let (l, holed) = if comment {
                (s.to_owned(), false)
            } else {
                self.holes(s, n, col0)?
            };
            self.modified |= holed;
            let at = |off: usize| col0 + chars(&l, off);
            let syntax = |w: &Self, msg: String| w.diag(Code::TemplateSyntax, n, col0, msg);
            let no_children = |w: &Self| match n.children.is_empty() {
                true => Ok(()),
                false => Err(syntax(
                    w,
                    "unexpected indentation: this line takes no block".into(),
                )),
            };
            let stmt = classify(&l, self.lang);
            if !matches!(stmt, Stmt::Command) {
                self.modified = true;
            }
            match stmt {
                Stmt::Var(name, e, off) => {
                    no_children(self)?;
                    let v = self.eval(e, false).map_err(|x| {
                        self.diag(
                            x.code,
                            n,
                            at(off + x.at),
                            format!("`{}`: {}", clip(e), x.msg),
                        )
                    })?;
                    self.env.locals.push((name.to_owned(), v));
                }
                Stmt::Unsupported(what) => {
                    return Err(syntax(
                        self,
                        format!(
                            "`{what}` is not supported by Beacon's template expander; expand it by hand or port it to a generator"
                        ),
                    ));
                }
                Stmt::Define(name) => {
                    let (d, _) = self.sub_dest(dest, name, n, col0)?;
                    let lines = self.block(c, &d, Mode::Normal)?;
                    self.files.push(file(d, lines));
                }
                Stmt::Function(prefix, name) | Stmt::Schedule(prefix, _, name) => {
                    if mode == Mode::Unpack {
                        return Err(syntax(
                            self,
                            "`function <..>:` and `schedule .. <..>:` are not allowed after UNPACK:SUBFUNCTION (use `definefunction`)".into(),
                        ));
                    }
                    let (d, call) = self.sub_dest(dest, name, n, col0)?;
                    let text = match stmt {
                        Stmt::Schedule(_, args, _) => format!("{prefix}schedule {args} {call}"),
                        _ => format!("{prefix}function {call}"),
                    };
                    let header = self.line(text, n, &cols, true)?;
                    let lines = self.block(c, &d, Mode::Normal)?;
                    self.files.push(file(d, lines));
                    out.push(header);
                }
                Stmt::For(v, a, b, step) => {
                    if step == 0 {
                        return Err(self.diag(
                            Code::TemplateExpr,
                            n,
                            col0,
                            "`for` step must not be zero",
                        ));
                    }
                    let (a, b, st) = (a as i128, b as i128, step as i128);
                    let count = if st > 0 {
                        (b - a + st - 1) / st
                    } else {
                        (a - b - st - 1) / -st
                    };
                    if count > MAX_LOOP as i128 {
                        return Err(self.diag(
                            Code::TemplateLimit,
                            n,
                            col0,
                            format!("loop runs {count} times (more than {MAX_LOOP})"),
                        ));
                    }
                    for k in 0..count.max(0) {
                        self.step(n, col0, 1)?;
                        let i = (a + k * st) as i64;
                        self.env.locals.push((v.to_owned(), Value::from(i)));
                        let lines = self.block(c, dest, mode)?;
                        out.extend(lines);
                        self.env.locals.pop();
                    }
                }
                Stmt::If(e, off) => {
                    let v = self.eval(e, true).map_err(|x| {
                        self.diag(
                            x.code,
                            n,
                            at(off + x.at),
                            format!("`{}`: {}", clip(e), x.msg),
                        )
                    })?;
                    if v == Value::Bool(true) {
                        let lines = self.block(c, dest, mode)?;
                        out.extend(lines);
                    }
                }
                Stmt::Foreach(i, v, e, off) => {
                    let list = self.eval(e, false).map_err(|x| {
                        self.diag(
                            x.code,
                            n,
                            at(off + x.at),
                            format!("`{}`: {}", clip(e), x.msg),
                        )
                    })?;
                    let Value::Array(list) = list else {
                        return Err(self.diag(
                            Code::TemplateExpr,
                            n,
                            at(off),
                            format!(
                                "`foreach` needs a list, `{}` is a {} (objects have no stable key order here)",
                                clip(e),
                                ty(&list)
                            ),
                        ));
                    };
                    if list.len() > MAX_LOOP {
                        return Err(self.diag(
                            Code::TemplateLimit,
                            n,
                            col0,
                            format!("loop runs {} times (more than {MAX_LOOP})", list.len()),
                        ));
                    }
                    for (k, x) in list.into_iter().enumerate() {
                        self.step(n, col0, 1)?;
                        self.env.locals.push((i.to_owned(), Value::from(k)));
                        self.env.locals.push((v.to_owned(), x));
                        let lines = self.block(c, dest, mode)?;
                        out.extend(lines);
                        self.env.locals.truncate(self.env.locals.len() - 2);
                    }
                }
                Stmt::Unpack => {
                    if parent != 0 || n.line != 1 {
                        return Err(syntax(
                            self,
                            "UNPACK:SUBFUNCTION can only be the first line of a file".into(),
                        ));
                    }
                    no_children(self)?;
                    mode = Mode::Unpack;
                    self.unpacked = true;
                }
                Stmt::Command => {
                    if mode == Mode::Unpack {
                        return Err(syntax(
                            self,
                            "commands are not allowed directly in a file using UNPACK:SUBFUNCTION"
                                .into(),
                        ));
                    }
                    // without a block it is a command (`say find the <key>:`), as in the reference
                    if !comment && !n.children.is_empty() && l.ends_with(">:") && !self.lang {
                        return Err(syntax(
                            self,
                            format!(
                                "malformed block header `{}` (names are [A-Za-z0-9_] segments joined by `/`)",
                                clip(&l)
                            ),
                        ));
                    }
                    if !comment && !n.children.is_empty() {
                        return Err(syntax(
                            self,
                            "unexpected indentation under a command".into(),
                        ));
                    }
                    let line = self.line(l, n, &cols, holed)?;
                    out.push(line);
                    // `#` comments may head an indented section
                    let lines = self.block(c, dest, mode)?;
                    out.extend(lines);
                }
            }
        }
        self.env.locals.truncate(mark);
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------
// expressions

struct ExprErr {
    /// Byte offset in the expression.
    at: usize,
    code: Code,
    msg: String,
}

/// Python keywords: any of them means syntax this evaluator does not take.
const KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
];

/// Values borrow from the scope and locals until something is computed: reads never copy.
struct Parser<'s, 'e> {
    s: &'s str,
    i: usize,
    depth: usize,
    /// Parsing the operand `and`/`or` short-circuits past: syntax is still checked (as Python
    /// compiles the whole expression), nothing is looked up, computed or charged.
    dead: bool,
    root: &'e Map<String, Value>,
    locals: &'e [(String, Value)],
    reads: &'e mut BTreeSet<String>,
    steps: &'e mut usize,
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Approximate size of a value in bytes (what a copy of it costs).
fn weight(v: &Value) -> usize {
    match v {
        // every value costs at least 8: a list of 10k `""` is not free to scan
        Value::String(s) => 8 + s.len(),
        Value::Array(a) => a.iter().map(weight).sum::<usize>() + 8,
        Value::Object(m) => m.iter().map(|(k, v)| k.len() + weight(v)).sum::<usize>() + 8,
        _ => 8,
    }
}

impl<'e> Parser<'_, 'e> {
    /// Copying or building `bytes` of value costs `bytes / COPY_BYTES` steps.
    fn charge(&mut self, bytes: usize) -> Result<(), ExprErr> {
        *self.steps += bytes / COPY_BYTES;
        match *self.steps > MAX_STEPS {
            true => Err(self.err(
                self.i,
                Code::TemplateLimit,
                format!(
                    "template does more than {MAX_STEPS} steps (copying values costs one step per {COPY_BYTES} bytes)"
                ),
            )),
            false => Ok(()),
        }
    }

    fn get(&mut self, name: &str) -> Option<Cow<'e, Value>> {
        match name {
            "True" => return Some(Cow::Owned(Value::Bool(true))),
            "False" => return Some(Cow::Owned(Value::Bool(false))),
            "None" => return Some(Cow::Owned(Value::Null)),
            _ => {}
        }
        let (locals, root) = (self.locals, self.root);
        if let Some((_, v)) = locals.iter().rev().find(|(n, _)| n == name) {
            return Some(Cow::Borrowed(v));
        }
        if let Some(v) = root.get(name) {
            self.reads.insert(name.to_owned());
            return Some(Cow::Borrowed(v));
        }
        // the reference's default scope, overridable by scope.json
        match name {
            "true" => Some(Cow::Owned(Value::Bool(true))),
            "false" => Some(Cow::Owned(Value::Bool(false))),
            _ => None,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.i += 1;
        }
    }

    fn err(&self, at: usize, code: Code, msg: impl Into<String>) -> ExprErr {
        ExprErr {
            at,
            code,
            msg: msg.into(),
        }
    }

    fn enter(&mut self) -> Result<(), ExprErr> {
        self.depth += 1;
        match self.depth > MAX_EXPR_DEPTH {
            true => Err(self.err(
                self.i,
                Code::TemplateLimit,
                format!("expression nests deeper than {MAX_EXPR_DEPTH} levels"),
            )),
            false => Ok(()),
        }
    }

    /// The token at the cursor, reported as syntax this evaluator does not take.
    fn unsupported(&self) -> ExprErr {
        let rest = &self.s[self.i..];
        let b = rest.as_bytes();
        let n = match b.first() {
            None => return self.err(self.i, Code::TemplateExpr, "unexpected end of expression"),
            Some(c) if is_word(*c) => b.iter().take_while(|c| is_word(**c)).count(),
            Some(c) if b"<>=!/%*&|^~.".contains(c) => b
                .iter()
                .take(2)
                .take_while(|c| b"<>=!/%*&|^~.".contains(c))
                .count(),
            Some(_) => rest.chars().next().map_or(1, char::len_utf8),
        };
        self.err(
            self.i,
            Code::TemplateExpr,
            format!(
                "unsupported syntax `{}`: templates evaluate names, numbers, strings, f-strings, `[..]`, parentheses, `+ - *`, comparisons, `in`, `and`/`or`/`not` and a few calls (port the rest to a generator)",
                &rest[..n]
            ),
        )
    }

    fn expect(&mut self, c: u8) -> Result<(), ExprErr> {
        self.ws();
        if self.peek() == Some(c) {
            self.i += 1;
            return Ok(());
        }
        Err(self.unsupported())
    }

    /// The keyword `word` at the cursor (whole word): consumed.
    fn kw(&mut self, word: &str) -> bool {
        self.ws();
        let end = self.i + word.len();
        let hit = self.s[self.i..].starts_with(word)
            && !self.s.as_bytes().get(end).is_some_and(|c| is_word(*c));
        if hit {
            self.i = end;
        }
        hit
    }

    /// `f` parsed with [`Parser::dead`] set when `skip`.
    fn skipping<T>(&mut self, skip: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        let was = self.dead;
        self.dead |= skip;
        let r = f(self);
        self.dead = was;
        r
    }

    /// A whole expression (Python's `or_test`): `or` and `and` return an operand and skip
    /// the right one when the left decides.
    fn test(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        self.enter()?;
        let mut v = self.and()?;
        while self.kw("or") {
            let skip = !self.dead && truthy(&v);
            let r = self.skipping(skip, Self::and)?;
            if !skip {
                v = r;
            }
        }
        self.depth -= 1;
        Ok(v)
    }

    fn and(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        let mut v = self.not()?;
        while self.kw("and") {
            let skip = !self.dead && !truthy(&v);
            let r = self.skipping(skip, Self::not)?;
            if !skip {
                v = r;
            }
        }
        Ok(v)
    }

    fn not(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        if !self.kw("not") {
            return self.comparison();
        }
        self.enter()?;
        let v = self.not()?;
        self.depth -= 1;
        Ok(Cow::Owned(Value::Bool(!truthy(&v))))
    }

    /// The comparison operator at the cursor, consumed.
    fn comp_op(&mut self) -> Option<&'static str> {
        self.ws();
        let op = match &self.s.as_bytes()[self.i..] {
            [b'=', b'=', ..] => "==",
            [b'!', b'=', ..] => "!=",
            [b'<', b'=', ..] => "<=",
            [b'>', b'=', ..] => ">=",
            [b'<', b'<', ..] | [b'>', b'>', ..] => return None,
            [b'<', ..] => "<",
            [b'>', ..] => ">",
            _ => {
                let at = self.i;
                if self.kw("in") {
                    return Some("in");
                }
                if self.kw("not") && self.kw("in") {
                    return Some("not in");
                }
                self.i = at;
                return None;
            }
        };
        self.i += op.len();
        Some(op)
    }

    /// `a op b`, one operator: it reads both sides, so it costs their size ([`COPY_BYTES`]),
    /// a dict's key lookup only the key.
    fn comparison(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        let a = self.arith()?;
        self.ws();
        let at = self.i;
        let Some(op) = self.comp_op() else {
            return Ok(a);
        };
        let b = self.arith()?;
        self.ws();
        let chain = self.i;
        if self.comp_op().is_some() {
            return Err(self.err(
                chain,
                Code::TemplateExpr,
                "chained comparisons are not supported (join them with `and`)",
            ));
        }
        if self.dead {
            return Ok(Cow::Owned(Value::Null));
        }
        match (op, &*b) {
            ("in" | "not in", Value::Object(_)) => self.charge(weight(&a))?,
            _ => self.charge(weight(&a) + weight(&b))?,
        }
        let r = compare(op, &a, &b).map_err(|m| self.err(at, Code::TemplateExpr, m))?;
        Ok(Cow::Owned(Value::Bool(r)))
    }

    fn arith(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        let mut v = self.term()?;
        loop {
            self.ws();
            let at = self.i;
            let op = match self.peek() {
                Some(op @ (b'+' | b'-')) => op,
                _ => break,
            };
            self.i += 1;
            let r = self.term()?;
            v = self.binary(at, op, &v, &r)?;
        }
        Ok(v)
    }

    /// `a op b`, charged by the size of the result.
    fn binary(
        &mut self,
        at: usize,
        op: u8,
        a: &Value,
        b: &Value,
    ) -> Result<Cow<'e, Value>, ExprErr> {
        if self.dead {
            return Ok(Cow::Owned(Value::Null));
        }
        let v = arith(op, a, b).map_err(|(code, m)| self.err(at, code, m))?;
        self.charge(weight(&v))?;
        Ok(Cow::Owned(v))
    }

    fn term(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        let mut v = self.unary()?;
        loop {
            self.ws();
            let at = self.i;
            if self.peek() != Some(b'*') || self.s.as_bytes().get(at + 1) == Some(&b'*') {
                break;
            }
            self.i += 1;
            let r = self.unary()?;
            v = self.binary(at, b'*', &v, &r)?;
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        self.ws();
        let at = self.i;
        let Some(op @ (b'-' | b'+')) = self.peek() else {
            return self.postfix();
        };
        self.enter()?;
        self.i += 1;
        let v = self.unary()?;
        self.depth -= 1;
        if self.dead {
            return Ok(Cow::Owned(Value::Null));
        }
        let bad = |v: &Value| format!("bad operand type for unary {}: '{}'", op as char, ty(v));
        match (num(&v), op) {
            (Some(Num::I(i)), b'-') => i
                .checked_neg()
                .map(|i| Cow::Owned(Value::from(i)))
                .ok_or_else(|| self.err(at, Code::TemplateExpr, "integer overflow")),
            (Some(Num::F(f)), b'-') => Ok(Cow::Owned(float(-f))),
            (Some(Num::I(i)), _) => Ok(Cow::Owned(Value::from(i))),
            (Some(Num::F(f)), _) => Ok(Cow::Owned(float(f))),
            (None, _) => Err(self.err(at, Code::TemplateExpr, bad(&v))),
        }
    }

    fn postfix(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        let mut v = self.primary()?;
        loop {
            self.ws();
            let at = self.i;
            match self.peek() {
                Some(b'[') => {}
                Some(b'.') => {
                    v = self.method(v)?;
                    continue;
                }
                _ => return Ok(v),
            }
            self.enter()?;
            self.i += 1;
            let k = self.test()?;
            self.expect(b']')?;
            self.depth -= 1;
            if self.dead {
                continue;
            }
            v = match v {
                Cow::Borrowed(v) => index(v, &k).map(Cow::Borrowed),
                // computed values are small (numbers, short strings): not subscriptable anyway
                Cow::Owned(v) => index(&v, &k).map(|x| Cow::Owned(x.clone())),
            }
            .map_err(|m| self.err(at, Code::TemplateExpr, m))?;
        }
    }

    /// `(` [expr] `)` at the cursor: the argument, if any (one at most).
    fn args(&mut self) -> Result<Option<Cow<'e, Value>>, ExprErr> {
        self.expect(b'(')?;
        self.enter()?;
        self.ws();
        let a = match self.peek() {
            Some(b')') => None,
            _ => Some(self.test()?),
        };
        self.expect(b')')?;
        self.depth -= 1;
        Ok(a)
    }

    /// `name(arg)` (`recv` = `None`) or `recv.name(arg)`, from [`call`]: it reads its inputs
    /// and builds its result, so it costs both.
    fn call(
        &mut self,
        at: usize,
        name: &str,
        recv: Option<&Value>,
        arg: Option<&Value>,
    ) -> Result<Cow<'e, Value>, ExprErr> {
        if self.dead {
            return Ok(Cow::Owned(Value::Null));
        }
        self.charge(recv.map_or(0, weight) + arg.map_or(0, weight))?;
        let r = call(name, recv, arg).map_err(|(code, m)| self.err(at, code, m))?;
        self.charge(weight(&r))?;
        Ok(Cow::Owned(r))
    }

    /// `.name(..)` on `v`, the cursor on the `.`.
    fn method(&mut self, v: Cow<'e, Value>) -> Result<Cow<'e, Value>, ExprErr> {
        let at = self.i;
        self.i += 1;
        self.ws();
        let b = self.s.as_bytes();
        let end = self.i + b[self.i..].iter().take_while(|c| is_word(**c)).count();
        let name = &self.s[self.i..end];
        if !METHODS.contains(&name) {
            return Err(self.err(
                at,
                Code::TemplateExpr,
                format!(
                    "unsupported method `.{name}`: templates call only {}",
                    METHODS.join(", ")
                ),
            ));
        }
        self.i = end;
        let arg = self.args()?;
        self.call(at, name, Some(&v), arg.as_deref())
    }

    /// `f'..{expr}..'`, the cursor on the quote: holes are whole expressions (Python 3.12, so
    /// their strings may reuse the quote), printed like `str()`; `{{`/`}}` are braces.
    fn fstring(&mut self, at: usize) -> Result<Cow<'e, Value>, ExprErr> {
        let q = self.s.as_bytes()[self.i] as char;
        self.i += 1;
        let mut out = String::new();
        // checked before anything is copied: a huge literal fails here, not after building
        let push = |p: &Self, out: &mut String, s: &str| match out.len() + s.len() {
            n if n > MAX_VALUE_BYTES => Err(p.err(
                at,
                Code::TemplateLimit,
                format!("string longer than {MAX_VALUE_BYTES} bytes"),
            )),
            _ => {
                if !p.dead {
                    out.push_str(s);
                }
                Ok(())
            }
        };
        loop {
            let rest = &self.s[self.i..];
            let Some(j) = rest.find([q, '\\', '{', '}']) else {
                return Err(self.err(at, Code::TemplateExpr, "unterminated string"));
            };
            push(self, &mut out, &rest[..j])?;
            let c = rest.as_bytes()[j];
            self.i += j + 1;
            match c {
                b'\\' => {
                    return Err(self.err(
                        self.i - 1,
                        Code::TemplateExpr,
                        "escape sequences in strings are not supported",
                    ));
                }
                b'{' | b'}' if self.peek() == Some(c) => {
                    self.i += 1;
                    push(self, &mut out, if c == b'{' { "{" } else { "}" })?;
                }
                b'}' => {
                    return Err(self.err(
                        self.i - 1,
                        Code::TemplateExpr,
                        "single '}' is not allowed in an f-string",
                    ));
                }
                b'{' => {
                    let v = self.test()?;
                    self.ws();
                    match self.peek() {
                        Some(b'}') => self.i += 1,
                        Some(b'!' | b':') => {
                            return Err(self.err(
                                self.i,
                                Code::TemplateExpr,
                                "f-string conversions and format specs are not supported",
                            ));
                        }
                        _ => return Err(self.unsupported()),
                    }
                    if !self.dead {
                        let s = match &*v {
                            Value::String(s) => Cow::Borrowed(s.as_str()),
                            v => Cow::Owned(
                                py_str(v).map_err(|m| self.err(at, Code::TemplateExpr, m))?,
                            ),
                        };
                        push(self, &mut out, &s)?;
                    }
                }
                _ => break, // the closing quote
            }
        }
        if self.dead {
            return Ok(Cow::Owned(Value::Null));
        }
        self.charge(out.len())?;
        Ok(Cow::Owned(Value::String(out)))
    }

    fn primary(&mut self) -> Result<Cow<'e, Value>, ExprErr> {
        self.ws();
        let at = self.i;
        let b = self.s.as_bytes();
        match self.peek() {
            Some(c) if c.is_ascii_digit() => {
                let digits =
                    |i: usize| i + b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
                let mut end = digits(at);
                let frac =
                    b.get(end) == Some(&b'.') && b.get(end + 1).is_some_and(u8::is_ascii_digit);
                if frac {
                    end = digits(end + 1);
                }
                if b.get(end).is_some_and(|c| is_word(*c) || *c == b'.') {
                    return Err(self.err(
                        at,
                        Code::TemplateExpr,
                        "unsupported number literal (only `12` and `1.5` forms)",
                    ));
                }
                self.i = end;
                let t = &self.s[at..end];
                match frac {
                    true => Ok(Cow::Owned(float(t.parse().unwrap_or(f64::NAN)))),
                    false => t
                        .parse::<i64>()
                        .map(|i| Cow::Owned(Value::from(i)))
                        .map_err(|_| self.err(at, Code::TemplateExpr, "integer literal too large")),
                }
            }
            Some(q @ (b'\'' | b'"')) => {
                let body = &self.s[at + 1..];
                match body.find([q as char, '\\']) {
                    Some(j) if body.as_bytes()[j] == q => {
                        self.i = at + 1 + j + 1;
                        Ok(Cow::Owned(Value::String(body[..j].to_owned())))
                    }
                    Some(j) => Err(self.err(
                        at + 1 + j,
                        Code::TemplateExpr,
                        "escape sequences in strings are not supported",
                    )),
                    None => Err(self.err(at, Code::TemplateExpr, "unterminated string")),
                }
            }
            Some(c) if is_word(c) => {
                let end = at + b[at..].iter().take_while(|c| is_word(**c)).count();
                let w = &self.s[at..end];
                if matches!(b.get(end), Some(b'\'' | b'"')) {
                    if w == "f" {
                        self.i = end;
                        return self.fstring(at);
                    }
                    return Err(self.err(
                        at,
                        Code::TemplateExpr,
                        format!("unsupported string prefix `{w}` (only `f`)"),
                    ));
                }
                if KEYWORDS.contains(&w) {
                    return Err(self.unsupported());
                }
                self.i = end;
                self.ws();
                if self.peek() == Some(b'(') {
                    // a scope value of that name would shadow the builtin, as in Python
                    if !BUILTINS.contains(&w) || (!self.dead && self.get(w).is_some()) {
                        return Err(self.err(
                            at,
                            Code::TemplateExpr,
                            format!(
                                "unsupported call `{w}(..)`: templates call only {} and the methods {}",
                                BUILTINS.join(", "),
                                METHODS.join(", ")
                            ),
                        ));
                    }
                    let arg = self.args()?;
                    return self.call(at, w, None, arg.as_deref());
                }
                if self.dead {
                    return Ok(Cow::Owned(Value::Null));
                }
                self.get(w).ok_or_else(|| {
                    self.err(at, Code::TemplateExpr, format!("name '{w}' is not defined"))
                })
            }
            Some(b'(') => {
                self.enter()?;
                self.i += 1;
                let v = self.test()?;
                self.expect(b')')?;
                self.depth -= 1;
                Ok(v)
            }
            _ => Err(self.unsupported()),
        }
    }
}

enum Num {
    I(i64),
    F(f64),
}

impl Num {
    fn f(self) -> f64 {
        match self {
            Num::I(i) => i as f64,
            Num::F(f) => f,
        }
    }
}

fn num(v: &Value) -> Option<Num> {
    match v {
        Value::Bool(b) => Some(Num::I(*b as i64)),
        Value::Number(n) => n.as_i64().map(Num::I).or_else(|| n.as_f64().map(Num::F)),
        _ => None,
    }
}

fn float(f: f64) -> Value {
    Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// Python's type name.
fn ty(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn arith(op: u8, a: &Value, b: &Value) -> Result<Value, (Code, String)> {
    if let (b'+', Value::String(x), Value::String(y)) = (op, a, b) {
        if x.len() + y.len() > MAX_VALUE_BYTES {
            return Err((
                Code::TemplateLimit,
                format!("string longer than {MAX_VALUE_BYTES} bytes"),
            ));
        }
        return Ok(Value::String(format!("{x}{y}")));
    }
    let r = match (num(a), num(b)) {
        (Some(Num::I(x)), Some(Num::I(y))) => {
            return match op {
                b'+' => x.checked_add(y),
                b'-' => x.checked_sub(y),
                _ => x.checked_mul(y),
            }
            .map(Value::from)
            .ok_or((Code::TemplateExpr, "integer overflow".into()));
        }
        (Some(x), Some(y)) => {
            let (x, y) = (x.f(), y.f());
            match op {
                b'+' => x + y,
                b'-' => x - y,
                _ => x * y,
            }
        }
        _ => {
            return Err((
                Code::TemplateExpr,
                format!(
                    "unsupported operand type(s) for {}: '{}' and '{}'",
                    op as char,
                    ty(a),
                    ty(b)
                ),
            ));
        }
    };
    match r.is_finite() {
        true => Ok(float(r)),
        false => Err((Code::TemplateExpr, "result is not a finite number".into())),
    }
}

fn index<'v>(v: &'v Value, k: &Value) -> Result<&'v Value, String> {
    match (v, k) {
        (Value::Array(a), Value::Number(n)) if n.is_i64() => {
            let i = n.as_i64().expect("is_i64");
            let j = if i < 0 { a.len() as i64 + i } else { i };
            usize::try_from(j)
                .ok()
                .and_then(|j| a.get(j))
                .ok_or_else(|| "list index out of range".into())
        }
        (Value::Array(_), k) => Err(format!("list indices must be integers, not {}", ty(k))),
        (Value::Object(m), Value::String(s)) => m.get(s).ok_or_else(|| format!("KeyError: '{s}'")),
        (Value::Object(_), k) => Err(format!("KeyError: {} (keys are strings)", ty(k))),
        (v, _) => Err(format!("'{}' object is not subscriptable here", ty(v))),
    }
}

/// Python's truthiness.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

/// Python's `<` family: numbers (bools are 0/1) and strings (code point order = UTF-8 byte
/// order); `None` = the types do not order.
fn py_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        _ => match (num(a)?, num(b)?) {
            (Num::I(x), Num::I(y)) => Some(x.cmp(&y)),
            (x, y) => x.f().partial_cmp(&y.f()),
        },
    }
}

/// Python's `==`: `1 == 1.0 == True`, containers element-wise, other types differ (no error).
fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| py_eq(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        _ => {
            num(a).is_some() && num(b).is_some() && py_cmp(a, b) == Some(std::cmp::Ordering::Equal)
        }
    }
}

/// `x in hay`: substring, list element, dict key.
fn contains(hay: &Value, x: &Value) -> Result<bool, String> {
    match (hay, x) {
        (Value::String(h), Value::String(x)) => Ok(h.contains(x.as_str())),
        (Value::String(_), x) => Err(format!(
            "'in <string>' requires string as left operand, not {}",
            ty(x)
        )),
        (Value::Array(h), x) => Ok(h.iter().any(|e| py_eq(e, x))),
        (Value::Object(m), Value::String(k)) => Ok(m.contains_key(k)),
        (Value::Object(_), Value::Array(_) | Value::Object(_)) => {
            Err(format!("unhashable type: '{}'", ty(x)))
        }
        // JSON keys are strings: no other value is one
        (Value::Object(_), _) => Ok(false),
        (h, _) => Err(format!("argument of type '{}' is not iterable", ty(h))),
    }
}

fn compare(op: &str, a: &Value, b: &Value) -> Result<bool, String> {
    use std::cmp::Ordering::{Greater, Less};
    let ord = || {
        py_cmp(a, b).ok_or_else(|| {
            format!(
                "'{op}' not supported between instances of '{}' and '{}'",
                ty(a),
                ty(b)
            )
        })
    };
    Ok(match op {
        "==" => py_eq(a, b),
        "!=" => !py_eq(a, b),
        "<" => ord()? == Less,
        "<=" => ord()? != Greater,
        ">" => ord()? == Greater,
        ">=" => ord()? != Less,
        "in" => contains(b, a)?,
        _ => !contains(b, a)?,
    })
}

/// String methods a template may call (one argument for the first two, none for the rest).
const METHODS: &[&str] = &["startswith", "endswith", "capitalize", "upper", "lower"];
/// Builtins a template may call, with one argument.
const BUILTINS: &[&str] = &["len", "str", "int"];

/// `recv.name(arg)` ([`METHODS`]) or `name(arg)` ([`BUILTINS`]), as Python computes them on
/// JSON data; strings stay under [`MAX_VALUE_BYTES`].
fn call(name: &str, recv: Option<&Value>, arg: Option<&Value>) -> Result<Value, (Code, String)> {
    fn expr<T>(m: String) -> Result<T, (Code, String)> {
        Err((Code::TemplateExpr, m))
    }
    let long = |n: usize| match n > MAX_VALUE_BYTES {
        true => Err((
            Code::TemplateLimit,
            format!("string longer than {MAX_VALUE_BYTES} bytes"),
        )),
        false => Ok(()),
    };
    let Some(recv) = recv else {
        let Some(x) = arg else {
            return expr(format!(
                "{name}() takes exactly one argument here (0 given)"
            ));
        };
        return match (name, x) {
            ("len", Value::String(s)) => Ok(Value::from(s.chars().count())),
            ("len", Value::Array(a)) => Ok(Value::from(a.len())),
            ("len", Value::Object(m)) => Ok(Value::from(m.len())),
            ("len", x) => expr(format!("object of type '{}' has no len()", ty(x))),
            ("str", x) => py_str(x).or_else(expr).map(Value::String),
            (_, Value::String(s)) => {
                // ponytail: ASCII digits and a sign; Python also takes `_` and other digits
                let t = s.trim();
                let d = t.strip_prefix(['+', '-']).unwrap_or(t);
                match !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) {
                    true => t
                        .parse::<i64>()
                        .map(Value::from)
                        .or_else(|_| expr("integer too large".into())),
                    false => expr(format!(
                        "invalid literal for int() with base 10: '{}'",
                        clip(s)
                    )),
                }
            }
            (_, x) => match num(x) {
                Some(Num::I(i)) => Ok(Value::from(i)),
                // in range: the cast saturates nothing
                Some(Num::F(f)) if f.abs() < 9.2e18 => Ok(Value::from(f.trunc() as i64)),
                Some(Num::F(_)) => expr("integer too large".into()),
                None => expr(format!(
                    "int() argument must be a string or a real number, not '{}'",
                    ty(x)
                )),
            },
        };
    };
    let Value::String(s) = recv else {
        return expr(format!("'{}' object has no attribute '{name}'", ty(recv)));
    };
    match (name, arg) {
        ("startswith" | "endswith", Some(Value::String(p))) => Ok(Value::Bool(match name {
            "startswith" => s.starts_with(p.as_str()),
            _ => s.ends_with(p.as_str()),
        })),
        ("startswith" | "endswith", Some(x)) => {
            expr(format!("{name} first arg must be str here, not {}", ty(x)))
        }
        ("startswith" | "endswith", None) => expr(format!(
            "{name}() takes exactly one argument here (0 given)"
        )),
        (_, Some(_)) => expr(format!("{name}() takes no arguments (1 given)")),
        _ => {
            long(s.len())?;
            let r = match name {
                "upper" => s.to_uppercase(),
                "lower" => s.to_lowercase(),
                _ => capitalize(s).or_else(expr)?,
            };
            long(r.len())?;
            Ok(Value::String(r))
        }
    }
}

/// Python's `str.capitalize`: the first char titlecased, the rest lowercased in the context of
/// the whole string (a final sigma). Rust has uppercase only: a first char whose titlecase may
/// differ (digraphs, Georgian, anything uppercasing to several chars) is an error.
fn capitalize(s: &str) -> Result<String, String> {
    let Some(c) = s.chars().next() else {
        return Ok(String::new());
    };
    let mut up = c.to_uppercase();
    let (Some(u), None) = (up.next(), up.next()) else {
        return Err(format!(
            "capitalize() of a string starting with {c:?} is not supported"
        ));
    };
    if matches!(c, '\u{1C4}'..='\u{1CC}' | '\u{1F1}'..='\u{1F3}' | '\u{10D0}'..='\u{10FF}') {
        return Err(format!(
            "capitalize() of a string starting with {c:?} is not supported"
        ));
    }
    let lower = s.to_lowercase();
    // `c` lowercases alone the same way (a first char is never a final sigma)
    let skip: usize = c.to_lowercase().map(char::len_utf8).sum();
    Ok(format!("{u}{}", &lower[skip..]))
}

/// Python's `str()` of a JSON value; lists and objects have no single-line form here.
pub fn py_str(v: &Value) -> Result<String, String> {
    Ok(match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), ..) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (.., f) => py_float(f.unwrap_or(f64::NAN)),
        },
        Value::Array(_) | Value::Object(_) => {
            return Err(format!("cannot print a {}: index it to one value", ty(v)));
        }
    })
}

/// Python's `repr(float)`: shortest round-trip digits, `.0` kept, exponent form below 1e-4
/// and from 1e16 (`1e-05`, `1e+16`).
fn py_float(x: f64) -> String {
    if !x.is_finite() {
        return match (x.is_nan(), x > 0.0) {
            (true, _) => "nan".into(),
            (_, true) => "inf".into(),
            _ => "-inf".into(),
        };
    }
    let e = format!("{x:e}");
    let (m, exp) = e.split_once('e').expect("LowerExp has an exponent");
    let exp: i32 = exp.parse().expect("LowerExp exponent");
    if x == 0.0 || (-4..16).contains(&exp) {
        let s = format!("{x}");
        return if s.contains('.') { s } else { s + ".0" };
    }
    format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(e: &str, scope: Value) -> Result<String, String> {
        let nodes = [];
        let src = RelPath::parse("x.mcfunction").unwrap();
        let mut w = Walker {
            nodes: &nodes,
            source: &src,
            env: Env {
                root: scope.as_object().unwrap(),
                locals: vec![],
                reads: BTreeSet::new(),
            },
            files: vec![],
            dests: BTreeMap::new(),
            lang: false,
            modified: false,
            unpacked: false,
            lines: 0,
            bytes: 0,
            steps: 0,
        };
        w.eval(e, false).map_err(|x| x.msg).and_then(|v| py_str(&v))
    }

    #[test]
    fn python_str_of_values() {
        for (f, s) in [
            (1.0, "1.0"),
            (0.5, "0.5"),
            (-0.0, "-0.0"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (1.5e300, "1.5e+300"),
            (123456789012345678.0, "1.2345678901234568e+17"),
        ] {
            assert_eq!(py_float(f), s, "{f}");
        }
        assert_eq!(py_str(&json!(true)).unwrap(), "True");
        assert_eq!(py_str(&json!(false)).unwrap(), "False");
        assert_eq!(py_str(&json!(null)).unwrap(), "None");
        assert_eq!(py_str(&json!(2.0)).unwrap(), "2.0");
        assert_eq!(py_str(&json!(-3)).unwrap(), "-3");
        assert!(py_str(&json!([1])).is_err());
    }

    #[test]
    fn evaluates_the_subset() {
        let s = json!({"i": 7, "a": "x", "m": {"p": {"x": 1.5}}, "l": [1, 2, 3], "t": true});
        let e = |x: &str| ev(x, s.clone());
        assert_eq!(e("10*i").unwrap(), "70");
        assert_eq!(e("1 + 2 * 3 - -1").unwrap(), "8");
        assert_eq!(e("(1 + 2) * 3").unwrap(), "9");
        assert_eq!(e("a + 'b' + \"c\"").unwrap(), "xbc");
        assert_eq!(e("m['p']['x']").unwrap(), "1.5");
        assert_eq!(e("m ['p'] ['x'] * 2").unwrap(), "3.0");
        assert_eq!(e("l[-1] + l[0]").unwrap(), "4");
        assert_eq!(e("t + 1").unwrap(), "2");
        assert_eq!(e("true").unwrap(), "True");
        assert_eq!(e(" i ").unwrap(), "7");
        for bad in [
            "i / 2",
            "i if t else 0",
            "a.strip()",
            "a.upper",
            "r'{a}'",
            "i is 7",
            "1 < i < 9",
            "i ** 2",
            "max(l)",
            "i << 1",
            "l[3]",
            "m['q']",
            "a - 'b'",
            "nope",
            "'a\\n'",
            "[1]",
            "",
        ] {
            assert!(e(bad).is_err(), "{bad}");
        }
        assert!(
            e("9223372036854775807 + 1")
                .unwrap_err()
                .contains("overflow")
        );
        let deep = format!("{}1{}", "(".repeat(40), ")".repeat(40));
        assert!(e(&deep).unwrap_err().contains("nests deeper"));
    }

    fn run(text: &str, scope: Value) -> Result<Expansion, Diagnostic> {
        expand(
            text,
            &scope,
            &RelPath::parse("src/m/functions/t.mcfunction").unwrap(),
            &RelPath::parse("functions/m/t.mcfunction").unwrap(),
        )
    }

    #[test]
    fn range_is_end_exclusive() {
        let x = run(
            "for <i 1..4>:\n    say `eval:i`\nfor <j 3..0 -2>:\n  say `eval:j`",
            json!({}),
        )
        .unwrap();
        assert_eq!(x.files[0].text, "say 1\nsay 2\nsay 3\nsay 3\nsay 1");
        let m = &x.files[0].line_map;
        assert_eq!((m[0].out_line, m[0].src_line, m[0].computed), (1, 2, true));
        assert_eq!(m[0].src_cols, 4..16);
        assert_eq!(m[4].src_line, 4);
    }

    #[test]
    fn untouched_without_constructs_and_lexical_scope() {
        let x = run("say <x>\n\n## not a template\n", json!({})).unwrap();
        assert!(!x.modified && x.files.is_empty());
        // a loop variable ends with its loop
        let e = run("for <i 0..1>:\n    say a\nsay `eval:i`", json!({})).unwrap_err();
        assert_eq!(e.code, Code::TemplateExpr);
        assert_eq!(e.span.as_ref().unwrap().line, Some(3));
        let x = run("var v = n + 1\nsay `eval:v`", json!({"n": 1, "o": 2})).unwrap();
        assert_eq!(x.files[0].text, "say 2");
        assert_eq!(x.scope_reads, BTreeSet::from(["n".to_owned()]));
    }

    #[test]
    fn headers_and_comments() {
        let x = run(
            "# keep `eval:nope`\nexecute as @a run function <a/b>:\n    say 1\n    function <c>:\n        say 2\nschedule delay add 1t <d>:\n    say 3",
            json!({}),
        )
        .unwrap();
        let t: Vec<(&str, &str)> = x
            .files
            .iter()
            .map(|f| (f.dest.as_str(), f.text.as_str()))
            .collect();
        assert_eq!(
            t,
            [
                (
                    "functions/m/t.mcfunction",
                    "# keep `eval:nope`\nexecute as @a run function m/t/a/b\nschedule delay add 1t m/t/d"
                ),
                ("functions/m/t/a/b/c.mcfunction", "say 2"),
                ("functions/m/t/a/b.mcfunction", "say 1\nfunction m/t/a/b/c"),
                ("functions/m/t/d.mcfunction", "say 3"),
            ]
        );
        assert!(x.files[0].line_map[1].computed && !x.files[0].line_map[0].computed);
    }

    /// Expected values from CPython 3.12 (`str(eval(e, scope))`).
    fn table() -> Value {
        json!({"z": 0, "f": 0.0, "e": "", "l": [], "m": {}, "n": null, "one": 1, "s": "x",
            "big": [1], "big2": [1.0], "d": {"k": 1}, "d2": {"k": 1.0}, "t": true})
    }

    #[test]
    fn truthiness_and_short_circuit() {
        for (x, want) in [
            ("z or 'y'", "y"),
            ("s or nope", "x"), // never evaluated: no NameError
            ("z and nope", "0"),
            ("one and s", "x"),
            ("e or l or 'last'", "last"),
            ("z or e or n", "None"),
            ("e and d['nope']", ""),
            ("s or d['nope'] or nope.upper()", "x"),
            ("not z", "True"),
            ("not s", "False"),
            ("not not big", "True"),
            ("not f", "True"),
            ("not m", "True"),
            ("not n", "True"),
            ("not d", "False"),
            ("not 'a' in s", "True"),
            ("1 + 1 == 2 and 'k' in d", "True"),
            ("(z or one) * 3", "3"),
        ] {
            assert_eq!(ev(x, table()).as_deref(), Ok(want), "{x}");
        }
        // a skipped operand is still parsed: syntax and unsupported constructs are errors
        for bad in [
            "s or (1 +)",
            "s or a.strip()",
            "z and max(l)",
            "s or f'{s!r}'",
        ] {
            assert!(ev(bad, table()).is_err(), "{bad}");
        }
        assert!(
            ev("s and d['nope']", table())
                .unwrap_err()
                .contains("KeyError")
        );
    }

    #[test]
    fn comparisons_and_membership() {
        for (x, want) in [
            ("1 == 1.0", "True"),
            ("t == 1", "True"),
            ("'1' == 1", "False"),
            ("'1' != 1", "True"),
            ("big == big2", "True"),
            ("d == d2", "True"),
            ("l == m", "False"),
            ("n == n", "True"),
            ("n == 0", "False"),
            ("2 < 2.5", "True"),
            ("'a' < 'b'", "True"),
            ("'B' < 'a'", "True"),
            ("'é' > 'z'", "True"),
            ("t >= 1", "True"),
            ("one <= 1", "True"),
            ("'b' in 'abc'", "True"),
            ("'' in ''", "True"),
            ("1 in big", "True"),
            ("1.0 in big", "True"),
            ("t in big", "True"),
            ("'k' in d", "True"),
            ("'q' not in d", "True"),
            ("1 in d", "False"),
        ] {
            assert_eq!(ev(x, table()).as_deref(), Ok(want), "{x}");
        }
        for (bad, msg) in [
            (
                "'a' < 1",
                "'<' not supported between instances of 'str' and 'int'",
            ),
            ("n < 1", "'NoneType' and 'int'"),
            ("d <= d", "'<=' not supported"),
            ("1 in 'abc'", "requires string as left operand, not int"),
            ("l in d", "unhashable type: 'list'"),
            ("1 in 5", "argument of type 'int' is not iterable"),
            ("1 in n", "'NoneType' is not iterable"),
            ("1 < 2 < 3", "chained"),
        ] {
            let e = ev(bad, table()).unwrap_err();
            assert!(e.contains(msg), "{bad}: {e}");
        }
    }

    #[test]
    fn methods_and_builtins() {
        for (x, want) in [
            ("'abc'.startswith('ab')", "True"),
            ("'abc'.endswith('bc')", "True"),
            ("'hello WORLD'.capitalize()", "Hello world"),
            ("'éCOLE'.capitalize()", "École"),
            ("'ΑΣ'.capitalize()", "Ας"),
            ("''.capitalize()", ""),
            ("'ß'.upper()", "SS"),
            ("'ABC'.lower()", "abc"),
            ("len('héé')", "3"),
            ("len(big)", "1"),
            ("len(d)", "1"),
            ("str(1.0)", "1.0"),
            ("str(t)", "True"),
            ("int('  -12 ')", "-12"),
            ("int(2.9)", "2"),
            ("int(-2.9)", "-2"),
            ("int(t)", "1"),
            ("'a'.upper().startswith('A')", "True"),
            ("str(len(s)) + 'x'", "1x"),
            ("s .upper()", "X"),
            ("len ( s )", "1"),
        ] {
            assert_eq!(ev(x, table()).as_deref(), Ok(want), "{x}");
        }
        for (bad, msg) in [
            ("1 .upper()", "'int' object has no attribute 'upper'"),
            (
                "int('1.5')",
                "invalid literal for int() with base 10: '1.5'",
            ),
            ("int('x')", "invalid literal"),
            ("len(1)", "object of type 'int' has no len()"),
            ("'a'.startswith(1)", "first arg must be str"),
            ("'a'.upper(1)", "takes no arguments"),
            ("'ǆa'.capitalize()", "not supported"),
            ("str(l)", "cannot print a list"),
            ("len()", "exactly one argument"),
            ("len(s, s)", "unsupported syntax `,`"),
            ("'a'.strip()", "unsupported method `.strip`"),
            ("print(s)", "unsupported call `print(..)`"),
        ] {
            let e = ev(bad, table()).unwrap_err();
            assert!(e.contains(msg), "{bad}: {e}");
        }
        // a scope value shadows the builtin, as in Python: calling it is not supported
        assert!(ev("len(s)", json!({"len": 1, "s": "x"})).is_err());
    }

    #[test]
    fn f_strings() {
        for (x, want) in [
            ("f'a{s}b'", "axb"),
            ("f\"{d['k']}\"", "1"),
            ("f'{d['k']}'", "1"), // Python 3.12: a hole may reuse the quote
            ("f'{{x}}'", "{x}"),
            ("f'{one + 1}-{s.upper()}'", "2-X"),
            ("f''", ""),
            ("f'{f\"{s}\"}'", "x"),
            ("f'{ s }'", "x"),
            ("f'{t}{f}{n}'", "True0.0None"),
            ("f'{s}' + f'{one}'", "x1"),
            ("f'{\"}\"}'", "}"),
        ] {
            assert_eq!(ev(x, table()).as_deref(), Ok(want), "{x}");
        }
        for (bad, msg) in [
            ("f'{s!r}'", "conversions and format specs"),
            ("f'{s:>3}'", "conversions and format specs"),
            ("f'{s=}'", "unsupported syntax"),
            ("f'{'", "unterminated string"),
            ("f'{s'", "string prefix"),
            ("f'{}'", "unsupported syntax"),
            ("f'}'", "single '}'"),
            ("f'abc", "unterminated string"),
            ("f'{l}'", "cannot print a list"),
            ("f'\\n'", "escape sequences"),
            ("F'{s}'", "string prefix"),
        ] {
            let e = ev(bad, table()).unwrap_err();
            assert!(e.contains(msg), "{bad}: {e}");
        }
    }

    #[test]
    fn hostile_expressions_are_coded_errors() {
        let s = || table();
        let deep =
            |open: &str, close: &str| format!("{}1{}", open.repeat(5000), close.repeat(5000));
        for e in [
            deep("(", ")"),
            deep("f'{", "}'"),
            deep("not ", ""),
            deep("[", "]"),
            deep("len(", ")"),
            deep("-", ""),
            "(".repeat(5000),
            "f'{".repeat(5000),
        ] {
            let x = ev(&e, s());
            assert!(x.is_err(), "{}", clip(&e));
        }
        assert!(
            ev(&deep("(", ")"), s())
                .unwrap_err()
                .contains("nests deeper")
        );
        assert!(
            ev(&deep("f'{", "}'"), s())
                .unwrap_err()
                .contains("nests deeper")
        );
        assert!(
            ev(&deep("not ", ""), s())
                .unwrap_err()
                .contains("nests deeper")
        );
        // method chains are a loop, not recursion
        let chain = format!("s{}", ".upper()".repeat(5000));
        assert_eq!(ev(&chain, s()).as_deref(), Ok("X"));
        // a 1 MB f-string literal fails by the value cap, before it is built
        let mb = format!("f'{}{{s}}'", "x".repeat(1 << 20));
        assert!(ev(&mb, s()).unwrap_err().contains("longer than"));
    }

    fn lang(text: &str, scope: Value) -> Result<Expansion, Diagnostic> {
        expand(
            text,
            &scope,
            &RelPath::parse("src/m/texts/en_US.lang").unwrap(),
            &RelPath::parse("texts/en_US.lang").unwrap(),
        )
    }

    #[test]
    fn lang_templates() {
        // the function headers, `UNPACK:*` and `functiontree` are text in a `.lang`; `##` stays
        let plain = "UNPACK:SUBFUNCTION\nfunction <a>:\ndefinefunction <b>:\n## note\na=b\t## c\n";
        let x = lang(plain, json!({})).unwrap();
        assert!(!x.modified && x.files.is_empty());
        assert!(!has_lang_subfunctions(plain));
        assert!(!has_lang_subfunctions(
            "key=Press <E>:\nk2=`eval` x\n# for <i 0..2>:\n"
        ));
        assert!(has_lang_subfunctions("foreach <i v l>:\n  k=v\n"));
        assert!(has_lang_subfunctions("k=`eval:1`\n"));
        let x = lang(
            "## head\nfor <i 0..2>:\n    k.`eval:i`=`eval:i * 2`\nif <'a' in s and not z>:\n  ok=`eval:s.capitalize()`\nif <z>:\n  no=1\n",
            json!({"s": "abc", "z": 0}),
        )
        .unwrap();
        assert_eq!(x.files.len(), 1);
        assert_eq!(x.files[0].dest.as_str(), "texts/en_US.lang");
        assert_eq!(x.files[0].text, "## head\nk.0=0\nk.1=2\nok=Abc");
        // a header with a block is still an error (unexpected indentation), as in the reference
        let e = lang("function <a>:\n  x=1\nfor <i 0..1>:\n  y=1", json!({})).unwrap_err();
        assert_eq!(
            (e.code, e.span.unwrap().line),
            (Code::TemplateSyntax, Some(1))
        );
    }

    #[test]
    fn if_blocks() {
        let x = run(
            "UNPACK:SUBFUNCTION\nforeach <i b blocks>:\n    if <b.startswith('a:')>:\n        definefunction <f`eval:i`>:\n            say `eval:b`\n    if <i == 0>:",
            json!({"blocks": ["a:x", "b:y", "a:z"]}),
        )
        .unwrap();
        let t: Vec<_> = x
            .files
            .iter()
            .map(|f| (f.dest.as_str(), f.text.as_str()))
            .collect();
        assert_eq!(
            t,
            [
                ("functions/m/t/f0.mcfunction", "say a:x"),
                ("functions/m/t/f2.mcfunction", "say a:z")
            ]
        );
        // the reference matches `if <(.+)>:` greedily: `>` inside the condition is fine
        let x = run("if <2 > 1 >:\n  say y", json!({})).unwrap();
        assert_eq!(x.files[0].text, "say y");
        // `var` inside an `if` ends with it (lexical)
        let e = run("if <1>:\n  var v = 1\nsay `eval:v`", json!({})).unwrap_err();
        assert_eq!(
            (e.code, e.span.unwrap().line),
            (Code::TemplateExpr, Some(3))
        );
        // the condition's error points into it
        let e = run("say a\nif <1 < 'a'>:\n  say b", json!({})).unwrap_err();
        let sp = e.span.unwrap();
        assert_eq!(
            (e.code, sp.line, sp.column),
            (Code::TemplateExpr, Some(2), Some(7))
        );
    }
}
