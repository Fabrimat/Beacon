# Migration fixtures (phase 3)

Synthetic Regolith projects for `beacon migrate`. No production data. CI uses them without
Deno, Python, or Regolith: the `*.expected/` trees were generated once, locally, by the real
legacy filters, and are committed.

| Path | What |
|---|---|
| `modular_mc/` | Regolith project using ModularMC (`dataPath: ./packs/data`) |
| `modular_mc.expected/{BP,RP}` | Real ModularMC output for it |
| `system_template/` | Regolith project using system_template (`dataPath: ./data`) |
| `system_template.expected/` | Real system_template output: `BP/`, `RP/`, `data/` (files the filter created), `system_template_log.json` |
| `regolith_config/*.json` | `config.json` samples for profiles -> targets: `default` (development export), `packaging` (nested profile, local filter, `when`), `edu_world` (`world` export with `build: education`, `worlds`) |

## What the projects exercise

`modular_mc/packs/data/modular_mc/` (modules apply in sorted path order: `a_core`, then `b_extra`):

- `:auto` (keeps the sub-folder), `:autoFlat`, object target (`path`/`subpath`/`name`, extension
  replacement), directory target (trailing `/`);
- `jsonTemplate` with `::` expressions, `K()`, `__unpack__` (expression and literal, with
  `__value__`), scope from a JSON import; JSONC source;
- `textTemplate` (`{ts: ... :}`);
- conflict pairs `a_core` -> `b_extra`: `skip` (`BP/items/shared.item.json`), `merge`
  (`RP/textures/item_texture.json`), `overwrite` (`BP/recipes/golem.json`), `appendEnd`
  (`RP/texts/en_US.lang`);
- glob source (`glob/*.item.json`);
- relative import of a shared helper (`../_shared/lib.ts`) and
  `import scope from "../scope.json" with { type: "json" }`;
- a `process.env.FIXTURE_EDITION` branch (`BP/edition.json`);
- `SCRIPTS` in two modules, bundled in module order into `BP/scripts/main.js`
  (`esbuild.buildPath` set, as real projects do: without it ModularMC writes the bundle to
  `ROOT_DIR/BP/scripts/main.js`, outside the export);
- a trimmed `auto-map.ts` in the data dir.

`system_template/data/system_template/` (systems apply in sorted order: `golem`, then `ruby`):

- `AUTO`, `AUTO_FLAT`, `auto_map.json` with `replace_extension`;
- `json_template` with backtick expressions, `K()` keys, f-strings; scope layered from
  `scope.json`, the filter's `settings.scope` (`config_health`), `_scope.json`, and the entry;
- a list comprehension in `_map.py` (one entry per color: a generator candidate);
- conflict pairs `golem` -> `ruby`: `merge`, `overwrite`, `skip`, and the `.lang` default
  (append at end; system_template inserts a blank line);
- glob source; a `data/` target (`data/st_notes/golem.txt`);
- two `.mcfunction` targets in subfunctions syntax: `golem/waves.mcfunction` (`for` + `eval` over
  `waves`/`label` of `golem/_scope.json`, plus a `function <boss>:`: translated) and
  `golem/gated.mcfunction` (an `if`, which Beacon does not expand: frozen). Their expected files
  (`BP/functions/golem/**`) and log entries were derived by hand from the filter's rules, not
  produced by the pinned tools; `d3_migrate` checks them through Beacon's own expander and the
  equivalence check.
- Every system needs a `_scope.json`, even `{}`, or system_template ignores it.

## How the expected trees were generated

Tools (2026-10-16, Windows 11): Regolith 1.8.0, Deno 2.5.6, ModularMC from
`github.com/Nusiq/regolith-filters` commit `1f85bf56f383d0e20822aaacb982925f6bfcf84f`
(ModularMC changelog 1.1.0; esbuild from Deno's npm cache), system_template filter 3.16.0 with
`regolith-system-template` 1.4.0 and `regolith-json-template` 1.3.2 on Python 3.12.10 (an
existing Regolith venv). Nothing was downloaded: both filters ran from local caches.

Both runs used `regolith run` on a scratch copy of the fixture, never on this folder, with the
copy's `config.json` changed in two places only:

- the filter definition became a local one (`runWith` + a relative `script`), so Regolith
  needs no `regolith install` and no network;
- `profiles.default.export` became `{"target": "exact", "bpPath": "<out>/BP", "rpPath":
  "<out>/RP", "readOnly": false}`.

ModularMC (bash; `$S` = scratch dir, `$MMC` = the cached `modular_mc` folder):

```bash
cp -r fixtures/migrate/modular_mc "$S/mmc/proj" && cd "$S/mmc/proj"
# config.json: filterDefinitions.modular_mc = {"runWith": "deno",
#   "script": "<path of $MMC/main.ts relative to $S/mmc/proj>"}; export as above
FIXTURE_EDITION=bedrock regolith run default
cp -r "$S/mmc/out/BP" "$S/mmc/out/RP" fixtures/migrate/modular_mc.expected/
```

`$MMC` was `%LOCALAPPDATA%/regolith/filter-cache/f715d4b67fe349ae8c07d63af3eb218b/modular_mc`.
The env branch was evaluated with `FIXTURE_EDITION=bedrock`.

system_template (bash; `$ST` = a Regolith project's
`.regolith/cache/filters/system_template`, `$VENV` = the venv with
`regolith-system-template` 1.4.0):

```bash
cp -r fixtures/migrate/system_template "$S/st/proj" && cd "$S/st/proj"
mkdir -p local_filters/system_template && cp "$ST/main.py" local_filters/system_template/
# config.json: filterDefinitions.system_template = {"runWith": "python",
#   "script": "./local_filters/system_template/main.py"}; export as above
PATH="$VENV/Scripts:$PATH" regolith run default
E=fixtures/migrate/system_template.expected
cp -r "$S/st/out/BP" "$S/st/out/RP" $E/
cp -r "$S/st/proj/.regolith/tmp/data/st_notes" $E/data/   # files the filter added to data/
cp "$S/st/proj/system_template_log.json" $E/
find $E -type f | xargs sed -i 's/\r$//'
```

system_template writes with the platform's newlines (CRLF on Windows); the committed tree is
normalized to LF, like the rest of the repo (`.gitattributes`). ModularMC output was already LF.
Compare text outputs line by line, not byte by byte.

Regenerate after changing a fixture with the same steps; review the diff of the expected
tree before committing.
