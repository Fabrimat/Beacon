# ctxtruth-bedrock

A [ctxtruth](https://github.com/Fabrimat/ctxtruth) plugin (`apiVersion: 1`, name `bedrock`) that verifies
Minecraft Bedrock claims in agent docs against the [Beacon](../../README.md) CLI. Spec: `docs/ARCHITECTURE.md` 8.4.

```js
const bedrock = require("ctxtruth-bedrock");
const plugin = bedrock({ beacon: "beacon", timeoutMs: 10000 }); // both optional
```

| Check | Verifies | Beacon input |
|---|---|---|
| `bedrock/commands` | `beacon ...` subcommands and flags exist (suggestion = full corrected line) | `beacon --help --json` |
| `bedrock/config` | `namespace`, `targetVersion`, `editions`, output ids vs `config.json` (via `ctx.readFile`; not environment-dependent) | none |
| `bedrock/ids` | `ns:id`s (project symbols; vanilla ids and components), `@minecraft/*` modules, exports, members | `beacon symbols --json`, `beacon api --json` |

Rules: the `beacon` from the environment (option `beacon`: a path, or `[path, ...leadingArgs]`), argv arrays and no
shell; each CLI call runs at most once per `verify`, bounded by `timeoutMs` (default 10 s). A timeout, a failed
call, malformed JSON, an unexpected shape, or a missing API index (`beacon api` absent or no index installed) makes
the affected claims **unknown**, never a pass and never an error. `<!-- beacon:generated:start/end -->` regions are
ignored.

Beacon inputs (schema 1): `beacon api --json` = `{gameVersion, installed, modules: {"@minecraft/server": [versions]}, vanilla: {kind: count}}`
(is an index installed, which modules); `beacon api lookup <q> --json` = `{gameVersion, hits: [{path, ...}]}` per claimed
`minecraft:*` id or module export/member (exact `path` match after `#`; memoized per verify, at most 50 per verify, the
rest unknown). Only a project's own namespaces (from `symbols`) can mismatch; foreign namespaces are unknown.

```
npm ci && npm test
```
