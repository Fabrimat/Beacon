# Template fixtures (`.mcfunction` and `.lang` subfunctions)

Synthetic Beacon modules for `crates/engine/src/template.rs` (no production data). Each
`<case>/` is a module (`functions/<name>.mcfunction` or `texts/<name>.lang`, + `scope.json`);
`<case>.expected/` is what the reference `regolith_subfunctions` 1.4.0 writes under
`BP/functions/<case>/` (or `RP/texts/`) for it, generated once and committed.
`crates/engine/tests/f1_template.rs` expands each case with `functions/<case>/<name>.mcfunction`
(or `texts/<name>.lang`) as the template's destination and compares every file (the set of
paths too).

| Case | Shape |
|---|---|
| `init/` | top-level `for` (with step, negative start, negative step) + `eval`, `##` dropped, `#` kept unevaluated, no trailing newline |
| `warp/` | `UNPACK:SUBFUNCTION` + `definefunction` (nested name `nested/deep`), `for` and `execute .. run function <n>:` inside a definition, `schedule .. <n>:` |
| `story/` | `foreach` over a list + `var name = item ['identifier']` + `execute .. run function <`eval:name`>:`, trailing blanks, subscripts, float/bool/string `+` printing |
| `xp_bar/` | `UNPACK:SUBFUNCTION` + `definefunction`, `for <i 0..101>:` with `eval:10*i` |
| `letters/` | `if` with `.startswith`/`.endswith`/`or`, comparisons, f-strings (`{{`, methods in holes), `upper`/`lower`/`len`/`str`/`int` |
| `lang_keys/` | `.lang`: `foreach` + `var` + `eval` keys, `.capitalize()`, `for` with an eval bound, nested `foreach`, `##` kept, function headers as text, a tab comment |
| `lang_if/` | `.lang`: `if` with `in`/`and`/`or`/`not`, `<`/`>=`/`==`/`!=` (`if <i < 2 >:`), truthiness of `''`/`False`, an f-string |

Beacon is stricter than the reference in one visible way: a plain command ending in `>:` and
containing `<` (e.g. `say Press <E>:`) that is followed by an indented line is a
`TemplateSyntax` error and the file ships nothing. The same line without an indented child is
an ordinary command and the file is left untouched.

## How the expected trees were generated

Tools (2026-10-19, Windows 11): `regolith_subfunctions` 1.4.0 on Python 3.12 from an existing
Regolith venv (nothing downloaded). Each case ran alone in a scratch directory (the reference
shares one scope dict across files):

```bash
PY=<venv>/Scripts/python.exe; F=fixtures/template
for c in init warp story xp_bar letters; do
  mkdir -p "$S/$c/BP/functions/$c" "$S/$c/data/subfunctions"
  cp $F/$c/functions/*.mcfunction "$S/$c/BP/functions/$c/"
  cp $F/$c/scope.json "$S/$c/data/subfunctions/scope.json"
  (cd "$S/$c" && "$PY" -I -c "import sys; sys.argv=['x','{}']; import regolith_subfunctions as r; r.main()")
  cp -r "$S/$c/BP/functions/$c" "$F/$c.expected"
done
# `.lang` cases: the reference edits `RP/texts/*.lang` only with `edit_lang_files`
for c in lang_keys lang_if; do
  mkdir -p "$S/$c/RP/texts" "$S/$c/BP/functions" "$S/$c/data/subfunctions"
  cp $F/$c/texts/*.lang "$S/$c/RP/texts/"
  cp $F/$c/scope.json "$S/$c/data/subfunctions/scope.json"
  (cd "$S/$c" && "$PY" -I -c "import sys; sys.argv=['x','{\"edit_lang_files\": true}']; import regolith_subfunctions as r; r.main()")
  mkdir -p "$F/$c.expected" && cp "$S/$c"/RP/texts/*.lang "$F/$c.expected/"
done
find $F -path '*expected*' -type f | xargs sed -i 's/\r$//'
```

The reference writes platform newlines (CRLF on Windows); the committed trees are LF.
Regenerate after changing a case with the same steps and review the diff.
