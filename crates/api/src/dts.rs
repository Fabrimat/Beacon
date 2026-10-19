//! `.d.ts` exports via oxc: classes, interfaces, enums, functions, consts, type aliases and
//! their members, with the first doc paragraph and `@deprecated`.

use beacon_engine::api_index::{ApiItem, ApiKind};
use oxc::allocator::Allocator;
use oxc::ast::ast::{ClassElement, Declaration, Expression, MethodDefinitionKind, Statement};
use oxc::parser::Parser;
use oxc::span::{SourceType, Span};
use std::collections::BTreeMap;

pub struct Export {
    pub name: String,
    pub item: ApiItem,
    /// String initializers of an enum's members (vanilla ids), in source order.
    pub enum_values: Vec<String>,
}

fn one_line(src: &str, s: Span) -> String {
    let t = src[s.start as usize..s.end as usize]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    t.trim_end_matches(';').to_string()
}

/// First paragraph of a `/** ... */` comment, and whether it has an `@deprecated` tag.
fn doc(raw: &str) -> (Option<String>, bool) {
    let body = raw.trim_start_matches("/**").trim_end_matches("*/");
    let mut para = Vec::new();
    let (mut deprecated, mut open) = (false, true);
    for l in body.lines() {
        let l = l.trim().trim_start_matches('*').trim();
        deprecated |= l.starts_with("@deprecated");
        if open {
            if l.starts_with('@') || (l.is_empty() && !para.is_empty()) {
                open = false;
            } else if !l.is_empty() {
                para.push(l);
            }
        }
    }
    let d = para.join(" ");
    ((!d.is_empty()).then_some(d), deprecated)
}

/// Exports of one `.d.ts`; a syntax error is an error (a half-read index would hide names).
pub fn parse(src: &str, file: &str) -> Result<Vec<Export>, String> {
    let a = Allocator::default();
    let ret = Parser::new(&a, src, SourceType::from_path("x.d.ts").unwrap_or_default()).parse();
    if let Some(e) = ret.diagnostics.first() {
        return Err(format!("{file}: {e}"));
    }
    // attached-to token start -> JSDoc span (the last one before a token wins)
    let mut docs = BTreeMap::new();
    for c in &ret.program.comments {
        if src[c.span.start as usize..].starts_with("/**") {
            docs.insert(c.attached_to, c.span);
        }
    }
    let mut out = Vec::new();
    for st in &ret.program.body {
        let Statement::ExportDeclaration(e) = st else {
            continue;
        };
        let d = &e.declaration;
        let (doc, deprecated) = docs.get(&e.span.start).map_or((None, false), |s| {
            doc(&src[s.start as usize..s.end as usize])
        });
        let mut m = BTreeMap::new();
        let mut put = |name: Option<std::borrow::Cow<str>>, s: Span| {
            if let Some(n) = name {
                m.entry(n.into_owned()).or_insert_with(|| one_line(src, s)); // overloads: first wins
            }
        };
        let mut values = Vec::new();
        let (name, kind) = match d {
            Declaration::ClassDeclaration(c) => {
                for el in &c.body.body {
                    match el {
                        ClassElement::MethodDefinition(x)
                            if x.kind != MethodDefinitionKind::Constructor =>
                        {
                            put(x.key.static_name(), x.span)
                        }
                        ClassElement::PropertyDefinition(x) => put(x.key.static_name(), x.span),
                        _ => {}
                    }
                }
                (c.id.as_ref().map(|i| i.name.to_string()), ApiKind::Class)
            }
            Declaration::TSInterfaceDeclaration(i) => {
                use oxc::ast::ast::TSSignature as S;
                for s in &i.body.body {
                    match s {
                        S::TSPropertySignature(x) => put(x.key.static_name(), x.span),
                        S::TSMethodSignature(x) => put(x.key.static_name(), x.span),
                        _ => {}
                    }
                }
                (Some(i.id.name.to_string()), ApiKind::Interface)
            }
            Declaration::TSEnumDeclaration(en) => {
                use oxc::ast::ast::TSEnumMemberName as N;
                for mem in &en.body.members {
                    let name = match &mem.id {
                        N::Identifier(i) => i.name.to_string(),
                        N::String(s) | N::ComputedString(s) => s.value.to_string(),
                        N::ComputedTemplateString(_) => continue,
                    };
                    if let Some(Expression::StringLiteral(s)) = &mem.initializer {
                        values.push(s.value.to_string());
                    }
                    put(Some(name.into()), mem.span);
                }
                (Some(en.id.name.to_string()), ApiKind::Enum)
            }
            Declaration::FunctionDeclaration(f) => {
                (f.id.as_ref().map(|i| i.name.to_string()), ApiKind::Function)
            }
            Declaration::TSTypeAliasDeclaration(t) => (Some(t.id.name.to_string()), ApiKind::Type),
            Declaration::VariableDeclaration(v) => {
                for dc in &v.declarations {
                    if let Some(n) = dc.id.get_identifier_name() {
                        out.push(Export {
                            name: n.to_string(),
                            item: ApiItem {
                                kind: ApiKind::Const,
                                members: BTreeMap::new(),
                                deprecated,
                                doc: doc.clone(),
                            },
                            enum_values: Vec::new(),
                        });
                    }
                }
                continue;
            }
            _ => continue,
        };
        if let Some(name) = name {
            out.push(Export {
                name,
                item: ApiItem {
                    kind,
                    members: m,
                    deprecated,
                    doc,
                },
                enum_values: values,
            });
        }
    }
    Ok(out)
}
