# Beacon - Architecture

> Status: draft v0.4 (2026-10-15). Working name "Beacon"; npm scope / GitHub org / domain
> availability not yet verified (`@beaconmc/*`, `beaconmc` used as placeholders).
> v0.4 folds in a multi-reviewer architecture review: Beacon is fully independent of
> ModularMC (migration only, no runtime compatibility); routing simplified to three concepts;
> project trust, sandbox policy, and daemon transport specified; budgets per phase;
> speculative features moved to [BACKLOG.md](BACKLOG.md).

Beacon is an open-source, AI-oriented toolchain for Minecraft Bedrock and Education Edition
content: worlds, add-ons, skin packs. It is an independent alternative to the Regolith +
ModularMC / system_template workflow, with its own simpler project format and a one-time
migration path from those tools.

---

## 1. Goals and constraints

**Goals**

- One install, one button, for technical and non-technical users alike (at alpha, section 14).
- **Extremely optimized: low RAM, fast compilation.** Budgets in section 12.
- One engine, many frontends: desktop app, CLI, VS Code extension, LSP, MCP.
- AI-oriented: machine-readable everything, MCP as a first-class frontend,
  compiler-enforced guardrails.
- Multi-pack output: one project can emit N behavior/resource packs into one `.mcaddon`
  (or `.mcworld` / `.mctemplate`).
- Bedrock and Education Edition as first-class targets from the same project.
- Native compatibility with Mojang's minecraft-debugger.

**Hard constraints**

- **Independent tool.** Beacon never runs, embeds, or emulates ModularMC, system_template, or
  Regolith at build time. Those formats are inputs to `beacon migrate` only (5.6).
- Comply with the Bedrock-OSS [project-config-standard](https://github.com/Bedrock-OSS/project-config-standard).
  Extensions must be non-destructive: never change the type or meaning of an existing field,
  and keep what other tools expect on disk (e.g. a `manifest.json` in each `packs.*` folder).
- Open source, vendor-neutral: no company branding, templates, or internal filters.
- No fork of VS Code (11.1).
- No telemetry. Network use is limited to explicit downloads (toolchain, updates, plugins);
  update checks carry no identifiers and can be disabled.
- No hosted services: everything runs locally. Release infrastructure (downloads, update
  channel, winget/brew) is distribution, not a service.

**Non-goals (for now)**

- Java Edition.
- A general-purpose IDE, and reimplementing TypeScript language features (editors already have them).
- Everything in [BACKLOG.md](BACKLOG.md) (graph authoring, agent observability panel, game
  capture, cinematic editor, game inspector, native debug bridge, evals, Blockbench/Snowstorm).

---

## 2. Lessons from the existing workflow

### 2.1 Structural pain

| Pain today | Root cause | Beacon answer |
|---|---|---|
| Profiles need tmp-dir resets or one full run per profile | Filters mutate a shared copied tmp dir in sequence | Pure transforms over a VFS; build graph shared across targets |
| Slow, non-incremental builds | Every run copies everything, every filter re-reads disk | Hash cache with observed-input tracking; watch rebuilds only affected nodes |
| Go + TS + Python + env vars glue | Tools grew independently | One engine, one process |
| Build scripts copied into every project | Toolchain lives inside the project | Toolchain is a versioned dependency; `beacon migrate` runs codemods |
| UUIDs protected only by convention | No tool owns them | Lockfile owned by the tool |
| Templating inside JSON strings | Templates are strings, not code | Typed content files and generators |
| Broken cross-file references found in-game | No global view of identifiers | Symbol table; broken refs are diagnostics |

### 2.2 How map files are actually used (study of two production projects, ~100 map files, ~580 entries)

- ~85% of targets use auto-mapping keywords; flat auto-mapping alone is ~80%.
- 12% of map files are pure static, 62% static + options, 26% "dynamic"; of those, about
  half only inject data. **~15% contain real logic, and all of it is one-to-many**
  (one template -> N outputs, driven by a directory glob or a data list).
- Most-used option by far: conflict policy (~200 uses). Its purpose is almost always to
  **merge shared registry files**: `item_texture.json`, `terrain_texture.json`,
  `sound_definitions.json`, `sounds.json`, `blocks.json`, `.lang`, manifest, `tick.json`,
  `init` functions, the scripts entrypoint.
- Next: JSON templating (~170), mcfunction/lang subfunctions (~40), per-entry scope (~40),
  text replacements injecting data into TS source (~20).
- Recurring workarounds: data injected into `.ts` via placeholder replacement; scope computed
  by globbing assets (e.g. one sound definition per `.ogg`); paths keyed on the module folder
  name; near-identical entry lists repeated across modules; the same source listed twice to
  reach both BP and RP.

Conclusion: routing can be convention + declarations, with code only for one-to-many
generation. Most conflict policies disappear if registries are **derived** (5.3).

Measured on Dimension-Clash (259 ModularMC `_map.ts`, 2026-10-15):

- **0** external imports in map modules (no `npm:`, bare, `jsr:`, `https:`, `node:*`); one
  type-only `@minecraft/server` import. 47 relative imports, 8 of them to gitignored
  `generated/*.ts` files produced by a pre-build Node script.
- **0** filesystem writes and **0** `Deno.*` calls during map evaluation; one
  `process.env.BETA_MODE` read; no `Date` / `Math.random`.
- Source-tree writes happen **before** the build, in ~20 Node scripts (sound map generation,
  emissive MER texture sets via `pngjs`/`sharp`, manifest/lang rewrite-and-restore in
  `build.js`). These are exactly what generators (5.3) and plugins (5.10) replace.
- Game scripts import only `@minecraft/server`, `@minecraft/server-ui`,
  `@minecraft/live-events-creator-api`.

Conclusion: map code is pure data building. Importing npm packages from build-time code is
possible but bad practice; Beacon supports it (5.4) without optimizing for it.

### 2.3 User research (hypothesis, n=1 so far)

Question: "what 3 things would make you switch, and what would make you not even try?"
Ongoing; answers are anonymized and summarized here. One answer does not set priorities on
its own; more answers from the team, collaborators, and the wider community are needed.

| # | Profile | Wants | Dealbreaker |
|---|---|---|---|
| 1 | Designer / educator, non-coder, runs kids' creative camps | Intuitive setup of build profiles and custom processing steps; a **much simpler way to make cinematics** (all existing tools, the official Editor included, feel convoluted); editing of in-game photos; a tool that can **"see" the game** and show what is happening visually | **CLI-only or many setup steps.** UX/UI quality decides adoption; more power does not compensate |

Implications:

- The alpha (section 14) is app-first with a one-flow install (9). Earlier phases are tools
  for the team and agents; they are steps toward the alpha, not releases for this persona.
- Configuration is editable visually: targets, outputs, plugins, and `module.json` as forms in
  the app that write the same files, with validation. "Custom processing steps" for non-coders
  map to generators or templates picked from a gallery, not to writing a filter.
- Cinematic editor, game inspector, and capture are in the backlog. In-game photo editing is
  out of scope.

### 2.4 First users

The team and its collaborators, on their own production projects. Every phase exit (14) is
judged first by whether it makes their real work faster or safer. Phases before the alpha are
not marketed externally.

---

## 3. System overview

```
              +--------------------------------------------------------------+
              |                    beacon engine (Rust)                      |
              |  config | VFS (metadata + lazy content) | build graph | cache  |
              |  routing | registries | symbols | schemas | package | emit   |
              |  trust                                                       |
              |   +----------------------+     +-------------------------+   |
              |   | JS host (trait)      |     | bundler (esbuild sidecar|   |
              |   | QuickJS              |     |  -> rolldown later)     |   |
              |   +----------------------+     +-------------------------+   |
              +-------------------------------+------------------------------+
                                              |
      +-------------+-------------+-----------+-----------+-------------+
      |             |             |                       |             |
   desktop app    CLI         `beacon serve` (per-project daemon)     vscode
   (Tauri,       `beacon`      transports: LSP | MCP | event stream    extension
    React UI)                                                          (LSP client)
```

Rules:

- **The engine is a library and a binary.** No frontend reimplements build, validation,
  or packaging. The CLI works without a daemon (cold path); it uses a running daemon when one
  exists.
- **One daemon per project (per worktree)**, `beacon serve`, shared by every client (app,
  VS Code, agents). Never one daemon per client.
  - **Transport**: a named pipe on Windows, a Unix domain socket elsewhere, owner-only ACL.
    LSP and MCP clients that expect stdio launch a thin `beacon serve --stdio` shim that
    attaches to the running daemon (starting it if needed). There is no TCP endpoint: the
    desktop app reaches the engine through Tauri IPC and per-call pipe connections (14.5).
  - **Discovery file** in the per-user runtime directory, never in the project:
    `%LOCALAPPDATA%\beacon\run\<project-hash>.json` (Windows),
    `$XDG_RUNTIME_DIR/beacon/` or `~/Library/Caches/beacon/run/` elsewhere, owner-only. It
    holds pipe path, optional port + token, pid, engine version, protocol version.
  - **Token**: random per daemon start, constant-time comparison, scoped (read: events and
    diagnostics; write: edits, build, deploy). Never in query strings, never written into AI
    client configs.
  - **HTTP hardening** (any TCP endpoint): `Host` allowlist (`127.0.0.1:<port>`,
    `localhost:<port>`), reject missing or foreign `Origin`, no CORS headers. Defends against
    DNS rebinding from a browser.
  - **Lifecycle**: an exclusive OS lock held for the daemon's whole lifetime (not only at
    startup) prevents double start; stale discovery files are detected by the lock and a
    handshake, never by pid alone. A client whose `beacon.version` or protocol version differs
    from the daemon's restarts it or refuses. Idle shutdown when no clients remain.
- **Single-writer document model.** The daemon owns the in-memory state of editable
  documents with version numbers. App, editor plugins, and agents submit edits against a base
  version; stale edits are rejected and rebased by the client. The daemon writes to disk.
  When the same file is open in an editor buffer: clean buffer -> the editor reloads; dirty
  buffer -> the user is asked which version wins.
- **Contributors**: every content-facing extension point is TypeScript (plugins, generators,
  app UI, VS Code extension, schemas, templates). Rust is needed only to work on the engine.

---

## 4. Project configuration

### 4.1 `config.json` (the standard) is the only config file

```jsonc
{
  "type": "minecraftBedrock",
  "name": "my-addon",
  "authors": ["Jane Doe"],
  "targetVersion": "1.21.100",
  "namespace": "demo",
  "packs": {
    "behaviorPack": "./packs/BP",      // the projected output (4.3)
    "resourcePack": "./packs/RP",
    "skinPack": "./packs/skins",       // when a skinpack target exists
    "worldTemplate": "./packs/WT"      // when a mctemplate target exists
  },
  "worlds": ["./worlds/*"],

  "beacon": {                          // tool-specific key, allowed by the standard
    "version": "0.4.0",
    "sources": ["./src"],
    "packMeta": {                      // names and descriptions (versions: 4.2.1)
      "name": "pack.name",             // lang key or literal
      "description": "pack.description",
      "packDependencies": "bp-rp"      // bp-rp | rp-bp | mutual: which manifest depends on its
    },                                 // sibling (uuid + released version, tracks releases);
                                       // mutual releases both packs of an output together
    "versionPolicy": "auto",           // auto | manual (4.2.1)
    "outputs": [ /* 4.3 */ ],
    "targets": { /* 4.4 */ },
    "plugins": []
  }
}
```

- Standard fields are read and written in standard form. Regolith-shaped `config.json`
  (`author`, `regolith.*`) is converted by `beacon migrate`, original kept until confirmed.
- `packDefinitions` (standard) is read as a symbol-table input. Beacon-derived ids are merged
  in memory and never written back into `config.json`.
- **Manifests: one source of truth.** `config.json` + `beacon.lock.json` are the source;
  every `manifest.json` (including the ones in the `packs.*` folders, kept so other tools work)
  is **generated output**, tracked in the emit ledger. `beacon check` reports a hand-edited
  manifest as drift.
- Every setting is also editable as a form in the app (6.1), writing the same file.

### 4.2 Lockfile: `beacon.lock.json`

Generated, committed, not hand-edited. Holds:

- Pack, module, and world template UUIDs keyed by **(output id x edition)**: Bedrock and
  Education builds get different UUIDs by default. `beacon.sharedEditionUuids: true` opts
  pack and module UUIDs into sharing one UUID across editions (some existing projects ship
  that way); world template header UUIDs stay per edition (separate store products).
- Script module UUID per output (used as the debugger's `targetModuleUuid`).
- Last released version and content hash per pack (each BP, RP, world template, skin pack).
- Resolved plugin sources (git URL + full commit SHA + content hash, 5.10) and schema set version.
- `canonicalVersion` (version of the canonical JSON writer) and the bundler name + version.
  Changing either changes content hashes, so it is an explicit `beacon migrate` step, never a
  silent effect of upgrading Beacon.

Output ids are stable keys: renaming an output goes through `beacon rename-output`, which
moves its lockfile entries instead of minting new UUIDs. UUID rotation: backlog.

**Dev UUIDs.** Dev builds use UUIDs derived deterministically from the lockfile UUID and a
worktree id (hash of the worktree path), never committed. Two worktrees deployed at once never
collide in the game, and dev builds never touch release identities.

#### 4.2.1 Pack versions

The game identifies a pack by UUID + version: shipping different content under the same
pair is the bug to prevent.

- **Each pack is versioned independently** (BP, RP, world template, skin pack have no reason
  to share a version).
- Dev builds never change versions (dev folders are not cached by the game; dev UUIDs, 4.2).
- **`versionPolicy: "auto"`** (default). Release builds (`beacon pack`) compare each pack's
  content hash with the last released hash in the lockfile: changed -> patch bump;
  unchanged -> same version. Overrides: `beacon pack --bump minor|major [--pack <id>]`.
- **`versionPolicy: "manual"`** (e.g. Marketplace submissions with externally controlled
  versions): `beacon pack --set-version <x.y.z> [--pack <id>]`. `beacon pack` fails if content
  changed and the version did not. Versions are monotonic per pack in both policies.
- **Content hash** is computed over the logical content (sorted file list + per-file hashes),
  never archive bytes, and excludes the pack's own version fields, UUIDs, and timestamps.
- **Dependency chain**: a BP manifest pins its dependencies' versions, so the BP's hash
  includes its generated manifest. When an RP is bumped, the dependent BP changes and is
  bumped too. Bumps are computed in dependency order. The world template is bumped when the
  world or any embedded pack changes. With `packDependencies: "mutual"` the sibling pins are
  left out of both hashes and the pair is decided together (either changed -> both bump once);
  switching an existing project to or from `mutual` bumps both packs once.
- `min_engine_version` is derived per target from `targetVersion` (Education lags Bedrock).
  Script module dependency versions (`@minecraft/server` etc.) come from the target's API
  index, not from pack versioning.

### 4.3 Outputs (multi-pack)

```jsonc
"outputs": [                                    // array order = stack order (see below)
  { "id": "game",     "include": ["src/game/**"], "dependsOn": ["mapA", "mapB", "lib"] },
  { "id": "mapA",     "static": { "bp": "./vendor/mapA/BP", "rp": "./vendor/mapA/RP" } },
  { "id": "mapB",     "static": { "bp": "./vendor/mapB/BP" }, "namespace": "mapb" },
  { "id": "lib",      "include": ["src/lib/**"], "namespace": "lib" }
]
```

Design inputs: community discussion on multi-pack support in Regolith (no "main pack" exists
in Minecraft's own model; pack order matters; symlink-based tmp folders are fragile; the
motivating case is combining several existing maps/add-ons, kept with their own namespaces,
scripts, and functions, plus new content depending on all of them).

- **All outputs are peers.** There is no main pack. For standard compliance, one output is
  **projected** into `packs.behaviorPack` / `packs.resourcePack`: `beacon.primaryOutput` if
  set, otherwise the first output with a BP. That is a projection, not a priority.
- An output is a **pair**: one BP and/or one RP. A module's BP content goes to the output's
  BP, its RP content to the output's RP. A module belongs to exactly one output.
- **Static outputs** include prebuilt packs as-is: their UUIDs and versions are kept, no
  transforms run on them, but they are validated and **indexed in the symbol table** so other
  outputs can reference their ids with autocomplete and broken-reference checks. Two packs
  with the same UUID (static or not) are an error.
- **Per-output namespace and scripts**: `namespace` overrides the project one; each BP output
  has its own script entrypoint and script module UUID. Cross-BP script communication is
  runtime messaging (e.g. `scriptevent`), not shared imports; shared script code is an
  npm/bundler concern.
- **Order semantics** (array order, first = highest priority):
  - **Resource packs**: true stack order; a higher pack overrides files of lower ones.
  - **Behavior packs**: order is the activation order only; duplicate identifiers across BPs
    are a conflict, not an override. The symbol table reports cross-output id collisions.
  - Written to `world_behavior_packs.json` / `world_resource_packs.json` of every emitted
    world, and used for dev deploy.
- **Dev deploy and order**: `development_*_packs` folders carry no order of their own; order
  comes from the world's pack list. The `dev` target therefore manages a **dev world**
  (`targets.dev.world`, default: a Beacon-created world per project) whose pack lists it
  writes on every deploy.
- No folder renaming, no symlinks (the VFS makes them unnecessary).
- Absent `outputs`: one implicit output mapped to `packs.behaviorPack` / `packs.resourcePack`.
- **Dependencies are declared** (`dependsOn`); each BP depends on its own RP by default.
  The symbol graph adds a **lint warning** when an output references ids defined only in an
  output it does not depend on. It never adds dependencies or fails the build on cycles:
  identifiers also hide in Molang, mcfunction text, and script strings, and all packs share
  one runtime namespace.
- Shared *content* modules across projects: backlog.

### 4.4 Targets (replaces Regolith profiles and per-edition flags)

```jsonc
"targets": {
  "dev":      { "edition": "bedrock",   "artifact": "dev", "world": "worlds/dev" },
  "addon":    { "edition": "bedrock",   "artifact": "mcaddon",    "outputs": ["*"] },
  "world":    { "edition": "bedrock",   "artifact": "mcworld",    "world": "worlds/main" },
  "world-nopacks": { "edition": "bedrock", "artifact": "mcworld", "world": "worlds/main", "embedPacks": false },
  "template": { "edition": "bedrock",   "artifact": "mctemplate", "world": "worlds/main" },
  "edu":      { "edition": "education", "artifact": "mcworld",    "world": "worlds/main",
                "targetVersion": "1.21.50" },
  "edu-dev":  { "edition": "education", "artifact": "dev" },
  "skins":    { "edition": "bedrock",   "artifact": "skinpack" },
  "store":    { "edition": "bedrock",   "artifact": "marketplace", "world": "worlds/main",
                "art": "./pack/art" },
  "beta":     { "edition": "bedrock",   "artifact": "dev", "define": { "BETA_MODE": true } }
}
```

- `targets.<t>.targetVersion` overrides the project one; schemas, symbols, API index, and
  edition transforms are version-aware per target.
- `define`: per-target build flags, read in generators as `ctx.define.X` (recorded as cache
  inputs). Replaces env-var flags such as `BETA_MODE`.
- `marketplace` artifact: the store submission zip
  (`Content/world_template/{behavior_packs,resource_packs,texts}`, `Store Art`,
  `Marketing Art`), as produced today by per-project CI scripts.
- Dev deploy per edition writes into that edition's `com.mojang` (Education:
  `%APPDATA%/Minecraft Education Edition/games/com.mojang`), detected by the setup wizard.
- `embedPacks: false` warns when the world references packs that will be missing.
- `beacon build --all` evaluates the shared graph once, then the per-target tail only.

### 4.5 Upstream RFC

Backlog (`additionalPacks`).

---

## 5. Engine

### 5.1 Pipeline

```
discover -> trust (5.5) -> index (VFS) -> route (5.3) -> generators (JS host)
         -> derive registries -> symbols + validate
         -> per target: edition transforms -> manifests/lang
         -> emit (dev dirs via ledger | archives)
```

### 5.2 VFS and cache (the RAM budget lives here)

- The VFS holds **path + size + hash for every file**. Content is loaded only for text files
  under a size cap, only while a transform needs it, then dropped.
- Binaries (textures, sounds, world `db/`) are never loaded: passed by path/hash and streamed
  straight to the dev dir or archive (stored, not deflated, for already-compressed formats).
  Large binaries are hardlinked/reflinked into dev dirs when on the same volume, copied otherwise.
- Hashing: blake3, with a (mtime, size, file-id) fast path; `--verify-hashes` forces full
  rehashing.
- **Cache keyed by observed inputs.** Every read by user code goes through host functions, so
  the engine records what each generator actually read. The key includes:
  - file contents read; **directory listings** (sorted names hash) for globs and `readDir`;
  - **negative lookups** (file probed but not found, failed module resolution);
  - imported modules transitively (bundled generator metafile, 5.4), npm packages by resolved
    version + integrity from the package lockfile;
  - env reads **with their values** (allowlisted, 5.4);
  - engine, JS prelude, oxc, esbuild, schema set, and `canonicalVersion`; target
    `targetVersion` and edition; relevant `config.json` options; plugin source hash.
  - `Date.now`, `Math.random`, `crypto.randomUUID` mark an evaluation uncacheable. External
    plugins always rerun.
- **Shared content-addressed cache** (safe for parallel agents): atomic writes (temp +
  rename), integrity check on read, size cap with LRU GC, cross-process locking. Phase 1
  ships a per-project cache without locking or GC (14.1).
- **Determinism**: file walks sorted by byte order of NFC-normalized UTF-8 paths; paths
  recorded as normalized relative paths; canonical JSON output (key order, number formatting,
  escaping; versioned as `canonicalVersion`); deterministic merge order for many-to-one
  targets; deterministic archives (fixed mtimes, entry order, permissions, compression level).
  Prelude freezes locale and timezone.

### 5.3 Routing: three concepts

Everything a user writes is one of three things. Stop at the first that works.

1. **Convention (zero config).** Folder = module; file suffix = destination
   (`sword.item.json` -> `BP/items/`, `sword.png` -> `RP/textures/items/`). Covers ~80-85%
   of files. The suffix table is published and versioned.
2. **`module.json` (optional, static, JSON Schema).** Exceptions only: explicit paths for
   non-standard files (fonts, UI, overridden vanilla textures), output assignment,
   `onConflict` (`skip` for defaults, `merge` for hand-written shared files). Editable by
   hand with autocomplete, or as a form in the app; both write the same file.
3. **Generators (`*.gen.ts`).** Code, only for one-to-many. A generator returns an array of
   content objects:

   ```ts
   // src/sounds/sounds.gen.ts
   export default generate(({ assets, ns }) =>
     assets("./*.ogg").map(f => sound(`${ns}:${f.name}`, { file: f.path })));
   ```

   - `assets(glob)` returns a typed list (name, path) and registers the glob as an input.
     `./` is the generator's own folder (like imports); `../` climbs at most to the module root.
   - `ctx.module` / `ctx.namespace` / `ctx.edition` / `ctx.target` replace path tricks and
     env flags.
   - A **content file** (one game concept split across packs) is a generator returning one item:

     ```ts
     // src/vehicles/car.entity.ts
     export default entity("demo:car", {
       server: { components: { "minecraft:health": { value: 20 } } }, // -> BP/entities
       client: { geometry: "./car.geo.json", texture: "./car.png" },  // -> RP/entity, RP/textures
       spawnRule: { /* ... */ },                                      // -> BP/spawn_rules
       lang: { en_US: "Car", it_IT: "Auto" },                         // -> texts/*.lang
     });
     ```

Automatic behaviors (not concepts the user configures):

- **Derived registries.** `item_texture.json`, `terrain_texture.json`, `sound_definitions.json`,
  `sounds.json`, `.lang` (+ `languages.json`), `tick.json`, and (for targets that still need
  it) `blocks.json` are **outputs computed from content declarations**, not files modules
  merge into.
  - Raw JSON files register too, by convention: a texture with the same stem next to a raw
    item/block file (`sword.png` beside `sword.item.json`) registers `ns:sword`; explicit
    entries in `module.json` cover the rest.
  - A duplicate key across modules is an **error**, overridable in `module.json`.
  - Non-derivable fields (`texture_name`, `padding`, `num_mip_levels`, sound event maps)
    have defaults and are overridable in `module.json`; hand-written shared files remain
    possible with `onConflict: merge`.
  - `blocks.json` is emitted only for targets older than the version where block JSON
    supersedes it (cutoff tracked in the schema set).
- **Hooks with order.** `init` and `tick` functions are declared with a priority; the order
  becomes the order of the generated arrays. Script entrypoints are aggregated automatically.
- **Data modules.** `import trophies from "./trophies.data.json"` in game scripts; the bundler
  inlines it, typed. Replaces text replacements.

The engine has **one content model** (a content item with routing metadata) and **one merge
primitive**; the three concepts are just three ways of producing content items.

### 5.4 JS host and sandbox

- **What runs**: generators, JS plugins, templates. Only code written for Beacon's API: no
  Deno or Node API compatibility layer.
- **Engine**: QuickJS (rquickjs) behind a trait. TS is stripped by oxc (syntax only, no type
  checking; `isolatedModules` rules apply: no `const enum`, no type-only re-exports without
  `type`). Pool of runtimes per thread, **one fresh context per evaluation** (no state or
  prototype leakage between modules).
- **npm imports**: a generator importing npm packages is pre-bundled by the esbuild sidecar
  (`platform: neutral`, host-virtual modules external) into a single ESM; the esbuild metafile
  becomes the import part of the observed inputs. The Rust module loader handles only relative
  imports, JSON, and host-virtual modules (`beacon:*`). No Node module resolution in Rust.
  `postinstall`/lifecycle scripts are never run by Beacon.
- **Host function policy** (the sandbox is exactly this list):
  - Filesystem: **read-only**, confined to the project root and `node_modules`. Paths are
    canonicalized and checked after resolution and again at open (TOCTOU); rejected: `..`
    escapes, absolute and UNC paths, symlinks/junctions leaving the root, Windows 8.3 short
    names, alternate data streams, reserved device names. Outputs only via returned content
    items, never direct writes.
  - Env: allowlist only (target/edition flags Beacon itself sets). No access to user secrets.
  - No network, no process spawning, no timers beyond a deterministic microtask pump.
- **Resource limits**: memory limit per runtime, stack limit, interrupt-handler time budget
  per evaluation, output size cap. A runaway generator becomes a coded diagnostic, never a
  hung daemon/LSP.
- **Claim**: capability-restricted, **not** hostile-code-proof. Untrusted projects do not run
  the host at all (5.5). Host functions are fuzzed in CI.
- Errors from user TS code are reported at the original source position (oxc/esbuild
  sourcemaps + QuickJS stack remapping).

### 5.5 Project trust

Opening a project must not execute its code until the user trusts it (same model as VS Code
Workspace Trust).

- **Restricted mode** (untrusted): parsing, schema validation, symbols, diagnostics, LSP
  navigation. Never runs: the JS host, external plugins, `legacy_filter`, git-URL plugin
  fetches, the esbuild sidecar, `npm` resolution, `beacon migrate`'s Python/Deno step.
- **Trust** is a per-user decision stored in the user profile, never in the project, keyed on
  the project path + a hash of `config.json`'s plugin list. Adding or changing a plugin asks
  again.
- Frontends: the app and VS Code show a trust prompt; CLI uses `--trust` or a prior trust
  decision; agents/CI pass `--trust` explicitly. Defaults never auto-trust.
- A project created by `beacon init` from a built-in template is trusted; from a git URL
  template, it is not, and its plugins are not installed until trusted.

### 5.6 Migration (one-time, never runtime)

- **`beacon migrate`** converts ModularMC, system_template, and Regolith projects into a
  native Beacon project, with a preview of all changes; the original is kept until confirmed.
- **Strategy**: evaluate the old map files **once**, record their concrete output (paths,
  contents, policies), then emit convention + `module.json` + generators + data modules:
  - static entries and pure data injection -> convention and `module.json`;
  - conflict policies on registry files -> dropped (derived registries);
  - one-to-many loops -> generator skeletons, with the evaluated output kept as the
    equivalence oracle;
  - env-flag branches -> `ctx.edition` or target `define` (4.4);
  - pre-build scripts that write generated sources (e.g. sound maps, texture sets) ->
    listed in the migration report as generators/plugins to port; their current output is
    imported as plain files meanwhile.
- **Evaluation runtimes, used by `migrate` only:**
  - ModularMC `_map.ts`: Beacon's own JS host (TS strip + relative imports + a
    `process.env` shim fed from the migration options). Measured maps need nothing more
    (2.2). Deno is downloaded on demand only as a fallback for maps using `Deno.*` or
    external imports.
  - system_template `_map.py`: Python via uv, on demand for that run only.
  - Both hash-pinned (9.2), never needed at build time.
- **Equivalence check**: the migrated project must build packs equal to the old tool's output
  after normalization (parsed JSON equal, text files exact, script bundles compared after
  sourcemap/path normalization). Differences are listed, not hidden.
- **Regolith filters** are not emulated. A `legacy_filter` plugin materializes the VFS to a
  temp dir, runs the executable (trusted code, scrubbed env, temp dir as cwd), and re-ingests
  the result. Survey the most-used community filters and decide which get native equivalents.

### 5.7 Symbol table

Entity/item/block ids, loot/trade paths, animation/controller/render-controller names, lang
keys, functions, families/tags (plus the standard's `packDefinitions`). Powers diagnostics,
cross-output lint and id-collision errors (4.3), LSP navigation/rename on string-literal
identifiers, MCP queries.

### 5.8 Schemas and types

- **Schemas: [Mojang/bedrock-schemas](https://github.com/Mojang/bedrock-schemas)** (MIT,
  (c) Microsoft; also on npm as `@minecraft/bedrock-schemas`). Official, generated by Mojang,
  JSON Schema draft-07, ships TS `.d.ts` types, editor form definitions, and a `catalog.json`
  (file glob -> schema). Tagged per game version, stable and preview separately
  (e.g. `v1.26.50.26`, `v1.26.60.29-preview`). Vendored by tag; the catalog seeds the
  convention suffix table (5.3) and the form definitions can drive the app's forms (6.1).
- Community sets (e.g. Blockception, BSD-3-Clause, with embedded third-party material) are
  used only for comparison or gap-filling after a provenance review.
- Coverage per file type is checked against the synthetic corpus in phase 0.
- `@minecraft/*` typings from npm (license checked per package). Versioned per `targetVersion`.

### 5.9 Built-ins

- Manifest and base lang generation (from config + lockfile, 4.1).
- Skin packs: `skins.json`, `texts/*.lang`, PNGs, `skin_pack` module.
- Worlds:
  - `.mcworld`: zip of the world folder root; `world_*_packs.json` written from output order.
  - `.mctemplate`: `manifest.json` with a `world_template` module and `base_game_version`,
    `level.dat`, `db/`, `texts/`, `world_icon.jpeg`; template options (e.g.
    `lock_template_options`) configurable per target.
  - `level.dat`: little-endian NBT with an 8-byte header; edited by validated round trip.
  - `.mcstructure` passthrough.
- **Edition transforms** (from The-Great-Adventure, which ships both editions from one
  Education-first source; differences are small):
  - `level.dat`, Bedrock target: `educationFeaturesEnabled=0`, `eduOffer=0`,
    `experiments.cameras=0`, `experiments.experiments_ever_used=0`,
    `experiments.saved_with_toggled_experiments=0`. Education target: `educationFeaturesEnabled=1`
    and the experiments its content needs (e.g. `cameras` for the camera item).
    `MinimumCompatibleClientVersion` from the target's `targetVersion`.
  - Manifests: `header.min_engine_version` per target (4.2.1); pack content otherwise identical.
  - World template: separate folder/name per edition, own header UUID (4.2) and product name;
    `world_*_packs.json` kept in sync with pack UUIDs and versions; `base_game_version`,
    `lock_template_options`, `allow_random_seed` per target.
  - Content usually needs **no** build-time branching: projects detect the edition at runtime
    (e.g. a command only Education accepts) and UI uses the vanilla `$education_edition`
    variable. `ctx.edition` is there for the rest.
  - Schemas/API index for the target's version; experimental toggles validated against it.
- Archive writers, pack validation.
- **Emit ledger** for dev dirs (`.beacon-emit.json`): delete stale files without touching the
  user's other packs. Writes go to a temp file in the same directory, then atomic replace;
  sharing violations (game holding a file) retry with backoff and end in a clear diagnostic.
  Long paths via `\\?\`; case-folding collisions between outputs and reserved names
  (`con`, `aux`, trailing dot/space) are diagnostics on every OS.

### 5.10 Plugins

- The transform contract is JS-facing: `{ id, inputs, outputs, run(ctx) }`. Built-ins may be
  native but obey the same declared-inputs/outputs contract.
- In-process JS plugins run in the JS host under the 5.4 policy.
- External-process plugins (JSON over stdin/stdout) are **trusted code**, like Regolith filters
  today: scrubbed env, temp dir as cwd, shown as "runs arbitrary code" when trusting.
- Sources: local path or git URL. Git plugins are **pinned to a full commit SHA** plus content
  hash in the lockfile; floating branches are rejected; updates only via
  `beacon plugin update` with a diff. Plugins declare capabilities (fs write paths, network,
  exec), shown at trust time. No registry for now.

---

## 6. Frontends

### 6.1 Desktop app

Tauri 2 shell + the React UI (Monaco, three.js previews). Primary entry point at the alpha.

- Project browser, file tree, Monaco, previews, lang table editor.
- **Play**: build `dev`, deploy, launch Minecraft. Content log, diagnostics, GameTest runner.
- First-run setup wizard and updater (section 9). Trust prompt (5.5).
- **Visual settings**: targets, outputs (drag-to-reorder stack order), plugins, pack metadata,
  and `module.json` as forms writing the same files. Nothing requires the CLI.
- Event stream content (diagnostics, content log, plugin output) is untrusted and sanitized
  before rendering in the webview.
- Platforms: Windows (WebView2, bootstrapper bundled for offline machines) first-class;
  macOS supported; Linux best effort (WebKitGTK; Bedrock does not run natively on Linux).
  CI runs on Windows and macOS; Linux builds are tested, not release-gated.

### 6.2 CLI: `beacon`

| Command | Phase (14) |
|---|---|
| `build [--target t \| --all]`, `watch`, `check`, `explain`, `--help --json` | 1 |
| `init`, `pack`, `fix`, `mcp` | 2 |
| `add <kind>`, `migrate`, `rename-output`, `plugin update` | 3 |
| `serve`, `api`, `dev`, `symbols`, `schema` | 4 |
| `test`, `doctor`, `setup` | 5 |

Every command supports `--json`. Non-interactive without a TTY; `--yes` and `--trust` where
confirmation exists; stable exit codes. JSON outputs other tools depend on (`--help --json`,
`symbols --json`, `api --json`, diagnostics) are versioned and only change compatibly within
a major version.

### 6.3 Diagnostics format

**SARIF 2.1** is the machine format (`--json`, LSP conversion, CI annotations, ctxtruth merge),
with `fixes` for fix-its. Fix-its are text edits shaped like LSP `WorkspaceEdit`, flagged
safe/unsafe; `beacon fix` applies safe ones by default. A compact text view is the default
for terminals and agents. Every diagnostic has a stable code; `beacon explain <code>`
returns the rule, an example, and the fix.

### 6.4 Language server (`beacon serve`)

Covers JSON content, lang, Molang, `module.json`, and cross-file string-literal identifiers.
Does not reimplement TypeScript features. Usable from VS Code, Zed, Neovim, JetBrains via the
stdio shim (3).

### 6.5 VS Code extension

Thin: LSP client, build/watch/dev commands, trust prompt, previews (webviews reusing the
app's React components), content log channel, debugger interop (6.6). Marketplace + Open VSX.
Before the LSP exists, a problem-matcher snippet gives in-editor errors from `beacon watch --json`.

### 6.6 Script debugging: interop with Mojang's minecraft-debugger

Upstream: [Mojang/minecraft-debugger](https://github.com/Mojang/minecraft-debugger), MIT,
actively maintained. A VS Code extension bridging DAP to Minecraft's script debugger over TCP
(default port 19144). Protocol details (`listen`/`connect` roles, wire framing,
`ProtocolVersion` negotiation, exact `launch.json` field names) are **unverified** and must be
checked against the upstream repo before implementation.

- **Interop**: correct sourcemaps; `launch.json` generated **per output** with the source-map
  roots and `targetModuleUuid` from the lockfile (dev UUID for dev builds). The VS Code
  extension configures the official debugger instead of duplicating it.
- Native bridge: backlog.
- Fork policy: port, do not hard-fork; keep Microsoft's copyright header and MIT notice
  (`NOTICE`); fixes go upstream first; no "Minecraft"/"Mojang" marks in Beacon names.

---

## 7. Visual graph authoring

Moved to [BACKLOG.md](BACKLOG.md) (B1). The engine keeps the hooks it would need
(generators, single-writer documents, symbol kinds).

---

## 8. AI-oriented design

### 8.0 Agent-first principles (apply to every component)

Every feature is designed for an agent loop (edit -> verify -> fix) first and a human UI
second; the UI is a view over the same commands.

- **Fast, scoped feedback.** `beacon check` has two tiers:
  - **fast** (default): schemas, symbols, derived registries, generated-section staleness;
    incremental, scoped to changed files/modules; p95 < 1 s on the large corpus;
  - **full** (`--full`): adds TypeScript type checking (tsgo) and other slow checks.
- **Machine-applicable fixes** (6.3).
- **Token-efficient by default.** Concise output, `--json` everywhere; MCP results are compact
  and paginated with stable cursors; schema lookups per component, never whole schemas; symbol
  queries instead of file reads; `check` over MCP returns the delta since the last call.
- **Small edit surface.** One file per game concept (5.3) and derived registries mean one
  change touches one file.
- **Non-interactive and deterministic.** No prompts without a TTY, stable exit codes,
  identical output for identical input.
- **Self-describing.** `beacon schema <component>`, `beacon symbols --find`, generated
  `AGENTS.md` and skills kept in sync with the project.
- **Parallel-agent safe.** One daemon per worktree, shared cache safe for concurrent use
  (5.2), dev UUIDs and dev deploy folders per worktree (4.2) so parallel agents never collide
  in the game.
- **The Beacon repo itself is agent-first**: `AGENTS.md`/`CLAUDE.md`, contributor skills, one
  command to build and test everything, fixtures as executable specs.

### 8.1 MCP

Transport of `beacon serve`: **stdio** via the shim (3). A read-only subset (`check`,
`symbols.*`, `schema.*`) ships early as a standalone stdio server (phase 2).

- Tool groups: `project.*`, `symbols.*`, `schema.*`, `api.lookup`, `build.*` / `check` /
  `pack`, `game.*` (deploy, reload, content log, GameTests).
- Target ~15-20 tools; each with annotations (read-only, destructive, idempotent), JSON
  schemas, an error model, and result-size caps. Write tools support `dryRun`. Full list in an
  appendix before phase 4.
- Outputs containing project or game text (lang strings, content log, chat, docs) are marked
  **untrusted** (prompt-injection surface). `game.*` deploy/reload require confirmation unless
  the client was explicitly granted it.

### 8.2 Guardrails (which layer enforces what)

- **Detectable invariant (the backstop).** `beacon check` fails when the lockfile, generated
  manifests, or generated doc sections were changed without the owning command (each records a
  hash of what it wrote). This holds no matter which agent or human made the change.
- **Blocking for Claude Code / Codex sessions.** A Beacon plugin with a `PreToolUse` hook
  that **denies** direct edits to protected files (lockfile, generated manifests, generated
  sections) and points to the right command; a `Stop` hook runs `beacon check`.
- **Policy.** "A task is done only when `beacon check` passes" lives in the generated
  `AGENTS.md`; it is a convention, enforced only by the hooks above.
- `beacon build --dry-run` shows the emitted diff.
- Toolchain downloads are hash-pinned and signed (9.2).

### 8.3 Generated agent context

`beacon init` / `migrate` generate `AGENTS.md` (+ `CLAUDE.md` pointer) and skills from the
actual project, between `<!-- beacon:generated:start -->` / `<!-- beacon:generated:end -->` markers. Generated regions are short and
deterministic (they are loaded every session). Regenerated on demand; `beacon check` fails if
stale.

### 8.4 Truth checks: no invented or outdated APIs, no stale agent docs

Two distinct problems, two homes.

**(a) Content and code correctness: inside the engine** (`beacon check`, LSP, MCP).

- **Versioned API index**, selected per target `targetVersion`, built from:
  - [Mojang/bedrock-samples](https://github.com/Mojang/bedrock-samples) `metadata/`:
    `script_modules/@minecraft/<module>_<version>.json`, `command_modules`, `molang_modules`,
    `vanilladata_modules` (blocks, items, entities, biomes, effects, ...), tagged per game
    version (stable and `-preview`). **License: "(c) Mojang AB. All rights reserved", use under
    the Minecraft EULA.** Beacon does not vendor or redistribute these files: the index is
    built locally from a pinned tag, downloaded on demand (like BDS), until a legal review
    says a derived index may ship.
  - `@minecraft/*` typings from npm and the vendored schema set (5.8).
  - Every index records its source tags/commits for provenance.
- JSON: unknown or misspelled components, outdated/deprecated `format_version`, experimental
  components without the matching toggle in the standard's `experimentalGameplay`.
- Scripts: members that do not exist (invented APIs), `@deprecated` or removed APIs across
  versions, beta APIs without the beta module, manifest script-module versions mismatching
  the npm typings.
- Molang queries and mcfunction commands checked against the version's grammar; vanilla
  identifiers checked against vanilla data.
- Prevention over correction: MCP `api.lookup` / `schema.*` give agents the exact signature
  and component shape **before** they write code.

**(b) Agent-facing docs: [ctxtruth](https://github.com/Fabrimat/ctxtruth)** (MIT, separate
public tool by the same author, run alongside Beacon, not inside it).

- ctxtruth extracts claims from `AGENTS.md`, `CLAUDE.md`, `README.md`, editor rule files and
  docs, and verifies them deterministically and offline: paths, package scripts, shell
  commands, TS symbols, type-checked snippets, package versions. Unknown never becomes an
  error. Outputs `agent` / `json` / `sarif` / PR comment; `--new-only` for PR drift.
- Beacon publishes **`ctxtruth-bedrock`**, a plugin for ctxtruth's plugin API (`apiVersion: 1`,
  types from `ctxtruth/plugin`). Default export: `(options) => plugin`, `name: "bedrock"`.

  | Check | Matches | `environment` | Verifies against |
  |---|---|---|---|
  | `bedrock/commands` | `commands: ["beacon"]` (shell fences + inline code) | true | `beacon --help --json`: subcommands and flags exist |
  | `bedrock/config` | inline code / prose facts (namespace, editions, `targetVersion`, output ids) | false | `config.json` via `ctx.readFile`, evaluated at every commit |
  | `bedrock/ids` | inline code matching namespaced ids, component names, `@minecraft/*` members | true | `beacon symbols --json` and `beacon api --json` for the current checkout |

  - `verify` is batched: each Beacon CLI call happens once per `verify`, no module-level cache
    (base and head alternate in one process).
  - Uses the `beacon` from the environment (option `beacon`), never a binary inside the analyzed
    checkout. `environment: true` checks become unknown on past commits, so `--new-only` and
    `blame` never attribute false drift.
  - The plugin declares `ignoreRegions` for `<!-- beacon:generated:start -->` / `<!-- beacon:generated:end -->`,
    so users need no extra config.
  - `suggestion` replaces the whole claim value: for commands it is the full corrected command
    line, not just the wrong flag.
  - ctxtruth puts no timeout on `verify`: the plugin bounds every Beacon CLI call itself and
    returns unknown on timeout.
  - The package exports a `default` (or `main`) condition, since ctxtruth resolves plugins with
    `require.resolve`. Report shapes follow ctxtruth's published JSON Schemas (v1).
  - Requires on the Beacon side: stable `--help --json`, `symbols --json`, `api --json` outputs
    (versioned JSON output policy, 6.2).
- Division of labor: Beacon **generates** the generated sections of agent docs (8.3, between
  `<!-- beacon:generated:start/end -->` markers) and fails `beacon check` if they are stale; ctxtruth
  **verifies** everything written by hand.
- ctxtruth needs Node; Beacon users do not. It is therefore optional for end users and
  standard in contributor/CI/agent environments: `beacon check --context` runs the built-in
  generated-section check always, and merges ctxtruth's SARIF when ctxtruth is available.
  ctxtruth is never part of a phase exit criterion.

### 8.5 In-game verification

`beacon test`: GameTests on BDS with structured results (Beta APIs toggle and
`server.properties` configured by Beacon; the runner drives the BDS console or a script
entrypoint). BDS downloaded with explicit, logged EULA acceptance, never redistributed.
Verifies behavior and scripts, never rendering. Eval suite, game capture, inspector: backlog.

---

## 9. Distribution and install

1. Install the app: `winget install beacon` / `brew install --cask beacon` / `.exe` `.dmg` `.AppImage`.
2. First-run wizard (defaults pre-selected, one "Continue"): detect Minecraft installs
   (Bedrock, Preview, Education; UWP vs GDK folder layouts), put `beacon` on PATH, offer
   (only if detected) the VS Code / Cursor / Windsurf extension and MCP registration for AI
   clients (config diff shown first; registers the stdio command, never a URL or token).
3. Create or open a project (trust prompt if not created locally), press **Play**.

The VS Code extension and `beacon setup` run the same wizard. `beacon doctor` fixes drift.

### 9.0 Project creation (`beacon init`): same flow from CLI, VS Code, and app

Project creation is fully doable from the **CLI** and the **VS Code UI**, without the app.

- **One engine, one declarative question spec.** The engine exposes the init steps as data
  (field, type, default, validation, condition). Each frontend only renders it:
  - CLI: interactive prompts on a TTY; `beacon init --name x --namespace y ...` or `--yes`
    for defaults; `--from init.json` for agents (non-interactive).
  - VS Code: "Beacon: New Project" opening a one-page form webview.
  - App: the same form.
- **Questions**: folder, name, namespace (validated, not `minecraft`), authors,
  `targetVersion` (from the vendored schema versions), editions, artifacts (-> targets),
  starter template (minimal add-on, world, skin pack, or a git URL: untrusted, 5.5), outputs
  (single by default), git init, agent setup (`AGENTS.md`/`CLAUDE.md`, MCP registration,
  diff shown before writing).
- **Result**: standard-compliant `config.json` with the `beacon` key, `beacon.lock.json`
  with UUIDs, folder skeleton, `.gitignore` (including `.beacon/`), `.vscode/` (`launch.json`
  per output, recommended extensions), agent context files. Then a first `beacon build dev`.
- **Existing folders**: opening a Regolith/ModularMC/system_template project offers
  **Migrate** (5.6), with a preview of the changes.
- One release train for all components. Projects pin `beacon.version`.

### 9.1 Dependencies

- **Users**: none besides Beacon and Minecraft. Deno or Python (via uv) only on demand, only
  for the `beacon migrate` run that needs them.
- **Contributors (engine)**: Rust stable. Clean engine build is minutes; this is contributor
  velocity, distinct from user compile time.
- **Contributors (everything else)**: Node LTS + pnpm.
- Key engine crates: oxc (strip/parse), rquickjs, esbuild sidecar binary (rolldown when its
  crate API stabilizes; a swap changes content hashes, 4.2), tower-lsp, MCP Rust SDK, zip
  writer, blake3, notify.
- Type checking (tsgo) only in `beacon check --full` and CI, optional download, never on the
  build/watch path.

### 9.2 Download trust

- Every downloaded binary (esbuild, tsgo, BDS, Deno, uv + Python builds, updates) is pinned
  by hash **embedded in the Beacon binary** for that release, never fetched from the same host
  as the file. Releases are signed (minisign or sigstore; key and rotation documented in
  `SECURITY.md`). TLS is transport only.
- First download asks for confirmation on a TTY / in the app; `--yes` or a config flag for
  non-interactive use.
- Code signing (Windows, Apple notarization), winget/brew manifests, update channel are
  prerequisites of the alpha.

### 9.3 Watch mode

`notify`-based. Handles Windows `ReadDirectoryChangesW` buffer overflow with a full rescan;
debounces and coalesces editor atomic saves; re-stats before reading and waits for size
stability on files being written (image editors, Blockbench). Ignores `.beacon/` and emit
directories inside the project. Falls back to polling on network drives and synced folders
(OneDrive), documented. Does not follow symlinks/junctions out of the project.

---

## 10. Repository layout

```
beacon/
  crates/
    engine/          config, vfs, graph, cache, routing, registries, symbols, trust, package, emit
    jshost/          JS host trait + QuickJS impl, host functions, module loader
    serve/           daemon: LSP + MCP + event stream, stdio shim
    cli/
  packages/          (TypeScript)
    runtime-api/     generator/content API (generate(), entity(), block(), assets(), ...)
    schemas/         vendored schemas + generated types
    ctxtruth-bedrock/
  apps/
    desktop/         Tauri + React
    vscode/
  templates/minimal/
  fixtures/
    synthetic/       public corpus generated from the 2.2 statistics (CI, perf budgets)
  docs/  ARCHITECTURE.md  BACKLOG.md  rfcs/
```

Production projects of the team are **never** in the repo and never run in CI: they are
tested locally by maintainers with a documented script (`pnpm fixtures:local <path>`) that
runs migrate + build + equivalence check and reports budgets.

Apache-2.0, DCO sign-off, CI: build, tests, synthetic corpus, perf budgets, BDS GameTests.

---

## 11. Key decisions

### 11.1 No VS Code fork

Monthly rebases on Code-OSS/Electron, losing the Microsoft Marketplace and every VS Code-based
editor that installs the extension from Open VSX. The app covers what a fork would add.

### 11.2 Rust engine + embedded QuickJS

- The resident processes (daemon beside a multi-GB game, LSP inside the editor, MCP) are where
  RAM matters. Node idles at ~40-50 MB and grows with a real project's VFS; a Rust daemon fits
  in tens. "TS first, Rust later" is rejected: the VFS/cache model is shaped by the language
  and the rewrite never happens.
- **No compatibility layer to carry.** Since Beacon runs only code written for its own API
  (1, 5.4), the JS host needs no Deno/Node emulation. QuickJS fits that workload (data
  building, not CPU-bound loops) at a small RAM and binary cost.
- **Perf fallback**: if generator evaluation time on the synthetic corpus misses the phase-1
  budgets, swap the trait implementation to V8 (deno_core). This is a perf fallback only, and
  it relaxes the binary and idle-RSS budgets (V8 adds tens of MB per isolate and to the
  binary); which budgets change is decided at that point, not hidden.
- Phase 0 spike measures real per-context RSS and evaluation time of representative generators
  instead of quoted figures.
- Bundling: esbuild sidecar, kept as one long-lived incremental service process in the daemon
  (no per-change spawn) if spawn cost ever misses a budget; phase 1 spawns the CLI per bundle
  and meets the watch budget (14.1); rolldown later. Narrow interface: entry points + externals +
  sourcemap -> files.
- App shell: Tauri, for install size and idle RAM.

### 11.3 `config.json` only

Standard file plus a `beacon` key. Programmatic needs go through generators and plugins.

### 11.4 Apache-2.0

Patent grant and contributor clarity.

### 11.5 SARIF for diagnostics

Existing standard, consumed by VS Code and CI, supports fixes, already emitted by ctxtruth.
No invented diagnostics schema.

---

## 12. Performance budgets

Measured on the public synthetic corpus in CI (large size: ~3,000 files, ~60 MB assets,
~20k LOC scripts) and reported by the local fixture script on production projects.

**Measurement rules**: RSS = sum over the process tree (Beacon + sidecars), because that is
what users see; on Windows, private working set. Idle = 30 s after the last build with no
client activity. Latencies are p95 over 50 runs, warm OS file cache unless stated. CI runs on
Windows with Defender enabled.

| Metric | Budget | Enforced from phase |
|---|---|---|
| Watch: single JSON change -> dev dir updated | p95 < 100 ms | 1 |
| Watch: single TS change -> dev dir updated | p95 < 300 ms | 1 |
| Build, warm cache / cold (no type-check) | < 2 s / < 5 s | 1 |
| Peak RSS during build (assets never loaded) | < 150 MB | 1 |
| Idle RSS, `watch` | < 40 MB | 1 |
| CLI start to first output (p50, warm) | < 50 ms | 1 |
| Binary size | < 30 MB (+ ~10 MB esbuild sidecar) | 1 |
| `check` fast tier, single-file change | p95 < 1 s | 2 |
| Idle RSS, `serve` (LSP + MCP + symbols + schemas) | < 100 MB | 4 |
| App idle RAM | < 150 MB | 5 |

Schemas and the API index are memory-mapped or lazily loaded; JS runtimes are dropped when
idle. A PR exceeding a budget beyond tolerance fails CI. Budgets are revisited once the phase 0
spike has real numbers.

---

## 13. Third-party licensing checklist

- [ ] Code ported from Mojang/minecraft-debugger (MIT, (c) Microsoft): keep headers, add to
      `NOTICE`; check `webview-ui` dependency licenses before reusing UI.
- [ ] Check whether upstream minecraft-debugger contributions require a Microsoft CLA.
- [x] Schemas: Mojang/bedrock-schemas, MIT (c) Microsoft. Keep copyright + license text in
      `NOTICE` / `THIRD_PARTY`.
- [ ] Mojang/bedrock-samples: "All rights reserved" + Minecraft EULA. Legal review before
      redistributing any file or derived index; until then fetch on demand, never commit.
- [ ] Community schema sets (Blockception, BSD-3-Clause + embedded material): audit before
      reusing anything.
- [ ] `@minecraft/*` npm packages: verify each license.
- [ ] "Not affiliated with Mojang or Microsoft" disclaimer.
- [ ] Runtime libraries bundled into generated add-ons: permissive, or optional with
      obligations documented.
- [ ] Fonts/assets: OFL / CC0 / permissive only.
- [ ] BDS: confirm EULA terms for automated download in CI.
- [ ] `LICENSE` (Apache-2.0), `CONTRIBUTING.md`, `SECURITY.md`, DCO, no-telemetry statement.

---

## 14. Roadmap

The phases are a process toward the **alpha**, which is the end of phase 5. Users of phases
1-4 are the team and its collaborators (2.4).

| Phase | Deliverable | Exit criterion |
|---|---|---|
| 0 | Repo, CI (Windows + macOS), synthetic corpus generator, perf harness, local fixture script; QuickJS spike on representative generators; corpus measurements from 2.2; name/org/domain | Perf harness reports every phase-1 metric; spike numbers recorded; go/no-go on QuickJS |
| 1 | Rust CLI `build/watch/check --json` (SARIF); VFS + cache; convention routing; `module.json`; generators in the JS host (sandbox policy, limits, esbuild pre-bundle); project trust; lockfile UUIDs + manifests + base lang; emit ledger; `.mcpack`/`.mcaddon`, one output | A new project made by hand builds and runs in-game; synthetic corpus builds; phase-1 budgets met |
| 2 | Derived registries, content files, hooks, data modules; targets with **Education Edition** transforms; **multi-pack** (outputs, static outputs, order, dev world); all archive types (`mcworld`, `mctemplate`, skin pack, `marketplace` submission zip); target `define`; `beacon init` / `add` / `pack` with `versionPolicy`; `AGENTS.md` generation; read-only stdio MCP | One `beacon build --all` produces Bedrock + Education artifacts for a multi-pack project; the team builds a real release of a new project with Beacon |
| 3 | `beacon migrate` (ModularMC + system_template, Deno/uv on demand, equivalence check); `legacy_filter`; `rename-output`, `plugin update`; Claude Code guardrail plugin (8.2) | Both production projects migrate locally and build equivalent packs, both editions, with no other tool at build time |
| 4 | `beacon serve` (daemon, stdio shim, LSP) + VS Code extension + debugger interop + versioned API index + full MCP + `ctxtruth-bedrock` | Rename of an entity id updates all references; breakpoints hit in TS source via the official debugger; `serve` idle budget met |
| 5 (**alpha**) | Desktop app (Play, visual settings and `module.json` forms, trust prompt) + setup wizard + `beacon test` on BDS + signing/distribution + updater | Clean Windows machine: install -> Play in under 5 minutes, no other installs; a non-coder completes it unaided |

After the alpha: [BACKLOG.md](BACKLOG.md), prioritized with evidence from the team and
collaborators.

### 14.1 Phase 1 as built (2026-10-16)

Decisions taken while building phase 1 that refine the sections above. Code is the source of
truth (`crates/engine/src/{path,model,host,diag,event}.rs` are the contract modules).

- **Sandbox = the VFS index (5.4).** User code reaches files only through `Vfs`, which serves
  only indexed paths. The walker never follows reparse points; reads re-check
  (size, mtime, file id) and the blake3 hash, returning "changed since indexing" otherwise.
  `NotFound` means "not indexed" only. Generators cannot read `node_modules` (npm
  pre-bundle of generators is deferred; measured maps have no npm imports, 2.2).
- **`assets(glob)`** is relative to the generator file's folder; `../` may climb only to the
  module root.
- **Cache (5.2), phase-1 form:** per-project `.beacon/cache/<blake3>.json` plus in-memory;
  key = generator + `GenCtx` + engine/canonical/suffix-table versions + `GeneratorHost::version()`
  (`HOST_ABI`); an entry is reused only if every observed file hash, directory listing hash,
  negative lookup, define and env value still matches. No cross-process lock or GC yet.
- **Bundling:** esbuild CLI spawned per bundle (~50 ms), no service protocol; reused while
  inputs are unchanged; bundles with inputs outside the project are never reused. esbuild is
  located via `BEACON_ESBUILD`, project `node_modules/@esbuild/<platform>`, or PATH, and only
  for trusted sessions (5.5). Hash-pinned download (9.2) is still to do.
- **Untrusted builds** of projects without code (no generators, scripts, or plugins) still
  build and write packs; any code makes `build` exit 3. Output directories reached through a
  link are refused when untrusted (warning when trusted).
- **Destinations:** no `--target` = packs projection (primary output to `packs.*`, others to
  `.beacon/out/<id>/`); `dev` = `development_*_packs/<name>-<worktree8> BP|RP` in the edition's
  com.mojang (`BEACON_COM_MOJANG` override); archives to `dist/`. `--out` overrides.
- **Watch:** stat-only incremental index updates, 6 ms coalescing, in-memory ledger;
  `watch --json` prints a `BuildEvent` after the build (ledger included) completes.
- **Measured (large corpus, Windows):** watch JSON p95 ~50-80 ms, TS p95 ~130-160 ms, cold
  ~2-3 s, warm ~0.8 s, idle RSS ~7 MB, binary 7.5 MB. CI enforces the phase-1 budgets on
  Windows and macOS and checks byte-identical `.mcaddon` output across both.

### 14.2 Phase 2 as built (2026-10-16)

- **Artifacts:** `dev`, `mcpack`, `mcaddon`, `mcworld`, `mctemplate`, `skinpack`, `marketplace`
  (`Content/world_template/`, optional `Content/skin_pack/`, `Store Art/`, `Marketing Art/`).
- **level.dat:** hand-rolled little-endian NBT; round-trips real Bedrock and Education files
  byte-identically. Edition table as in 5.9, then per-target `levelDat` dotted-path overrides
  (they win). `MinimumCompatibleClientVersion` = `[major, minor, patch, 0, 0]`.
- **World UUIDs:** `lock.worlds["<folder>"][edition]` and `lock.skins[...]`, keyed by folder,
  never shared across editions. A plain `.mcworld` has no manifest UUID.
- **Static outputs:** prebuilt packs passed through byte-identically, UUID/version read from
  their manifest, never bumped; duplicate pack UUIDs anywhere are an error and block the plan.
- **Pack lists:** `world_*_packs.json` in `beacon.outputs` order (first = top). RP stack
  direction in the client is still to be confirmed manually.
- **Managed dev world:** seeded once into `minecraftWorlds/<name>[-<target>]-<worktree8>/`;
  afterwards Beacon owns only its two pack-list files; a world folder without Beacon's
  ledger, or reached through a link, is refused. Two dev targets of one edition get separate
  folders but share dev UUIDs (deploy one at a time).
- **Derived registries:** `item_texture.json` / `terrain_texture.json` fragments (`ns:<stem>`)
  from sibling pngs, `item()/block({texture})`, and `module.json` `itemTextures` /
  `terrainTextures`; `hooks.tick` -> `functions/tick.json`; `hooks.init` ->
  `functions/beacon/init.mcfunction` (called by the project). `blocks.json` and `sounds.json`
  derivation are deferred.
- **Content files:** `*.entity.ts`, `*.item.ts`, `*.block.ts` default exports.
- **Versions (4.2.1):** only `beacon pack` changes release versions; `build` uses the last
  released version and warns `PackReleaseBehind`; dev never touches versions. Content hash and
  RP -> BP -> world template -> skin/marketplace order as specified; two targets producing
  different content for one release entry is an error.
- **Tools:** esbuild 0.28.2 is downloaded on demand (hash-pinned per platform, confirm via
  `--yes` / `BEACON_DOWNLOAD`, trusted sessions only). BDS 1.26.52.3 is pinned for
  `cargo bds smoke` (EULA acceptance logged); CI runs it on every push.
- **Fixtures target 1.26.50**: packs with `min_engine_version` newer than the released game
  are rejected by BDS and by clients.
- **`beacon init`** (question spec via `init --describe --json`, built-in `minimal` template,
  trusted), **AGENTS.md** generated region with `AgentDocsStale` + safe fix, **`beacon fix`**,
  **`beacon mcp`** (read-only stdio; tools `check`, `project_info`, `explain`; trust only from
  the stored decision or the command line). The `symbols.*` / `schema.*` MCP tools need the
  symbol table and API index and move to phase 4; `beacon add` moves to phase 3.
- **`pack` never releases dev targets**; with `sharedEditionUuids`, release records are
  still kept per edition (content differs by `min_engine_version` and edition transforms).
- Not yet evidenced: the second half of the phase-2 exit ("the team builds a real release of
  a new project") is a process step for the team.

### 14.3 Phase 3 as built (2026-10-19)

Contract: `crates/engine/src/migrate/` (`detect` -> `LegacyEval` -> `plan` -> `apply`,
`equiv::compare`), `crates/jshost/src/legacy.rs`; fixtures in `fixtures/migrate/` (expected
trees generated once by the real filters, see its README). Deviations from 5.6 and the
phase-3 row:

- **No Regolith in the binary.** Beacon never runs or emulates Regolith; profiles are read from
  `config.json` and mapped to targets once.
- **`--oracle` is previously built output** (a legacy `BP/` + `RP/` the user built with the old
  tools), not something `migrate` produces by running them.
- **No Deno / uv pins in phase 3.** ModularMC maps are evaluated in Beacon's JS host;
  system_template `_map.py` is not evaluated by Beacon: its real output is passed with
  `--evaluated` (Regolith tmp layout plus `system_template_log.json`).
- **No generator emission.** One-to-many loops are imported as their evaluated output and listed
  as generator candidates in the report.
- **`beacon plugin update`** moves to phase 4.
- **`legacy_filter`** (`crates/engine/src/legacy_filter.rs`) is minimal and non-blocking: a
  Regolith filter kept as an external-process plugin, reported as `MigrateToPort`, not required
  for the phase-3 exit. Config: `beacon.plugins: [{"legacyFilter": {"run": ["python",
  "filter.py"], "settings": {}, "stage": "afterMerge", "cwd": "<filter dir>", "timeoutSecs":
  120}}]` (only `afterMerge`; a relative `run` argument naming a file of `cwd` is made absolute).
  It is trusted code: it runs only in a trusted session (`beacon trust` shows plugins as
  "runs arbitrary code"; the plugin list is part of the trust hash). Per non-static output the
  merged items are written to a fresh temp dir as `BP/`, `RP/` and an empty `data/`; the command
  runs there (cwd = that dir) with a scrubbed env (`PATH`, `SystemRoot`, `TEMP`, `TMP`,
  `USERPROFILE`, `HOME`, `PYTHONUTF8=1`, `PYTHONDONTWRITEBYTECODE=1`, `FILTER_SETTINGS`) and the settings JSON as its last
  argument, like Regolith. Non-zero exit or timeout = `LegacyFilterFailed`: an Error diagnostic, and the plan writes no artifacts. The timeout kills only the child process, not grandchildren it spawned. Output caps: 64 MiB per file, 512 MiB total (`maxFileBytes`/`maxTotalBytes` override them), over a cap = `LegacyFilterFailed`; only the last 64 KiB of the filter's log is read. After the filter, each pack's generated `manifest.json` is restored (a Note if the filter changed it), so UUIDs and versions never change. Only
  `BP/` and `RP/` are read back (every path through `RelPath::parse`, no link followed, bytes
  read into `Body::Text`/`Bytes`, never passed through); other top-level entries are ignored
  with a note, and any escape (bad path, link, a write outside the work dir) is
  `LegacyFilterEscaped` and the whole output is discarded. The data dir is not synced back.
- **Local production check** (`scripts/fixtures-local.ps1`, `.sh`; maintainer only, never in
  CI, `-WhatIf`/`--dry-run` prints the steps). It copies a real Regolith project to
  `%TEMP%\beacon-mig\<name>` (source untouched; nothing is written in the repo), appends a
  `beacon_oracle` profile (the packaging filters, `exact`-exported to `./.beacon-oracle/{BP,RP}`)
  and runs it with Regolith; for system_template projects it also runs `beacon_eval`
  (`subfunctions`, `system_template`, `shapescape_custom_blocks_generator`,
  `system_template_esbuild`, exported to `./.beacon-eval/` with the retained tmp `data/` and
  `system_template_log.json`), then `beacon migrate <copy> --out <tmp>/migrated --oracle ...
  [--evaluated ...] --trust --json` and prints the summary, `MIGRATION.md` and the JSON report
  (the diff list).
- **Oracle rules.** The oracle is output the legacy toolchain built from the same filters, never
  Beacon's own; both it and the migrated build are compared per file (text by lines, JSON by
  value, binaries by hash) and every difference is listed, none hidden. The migrated project
  must build with Beacon alone. Production data stays in `%TEMP%`.
  `--oracle-profile <profile>` builds the migrated target that profile maps to (a world-free
  target with the same edition and `targetVersion` stands in when a world folder is missing;
  BP/RP are extracted from archives by manifest module type). Ignored: identity fields of
  manifests (`header`/`modules` uuid and version, sibling uuid), source maps, files at the
  oracle root, `data/**`; bundles compare module order through the `// path` headers.
- **Real-run results (exit check).** Dimension-Clash (ModularMC, Bedrock): 3134 files, 0 diffs.
  The-Great-Adventure (system_template, `packaging` = Education and `bedrock_packaging` =
  Bedrock): 2942 files, 1 diff each, the bundle's module-path comments (the legacy
  `text_replacer` rewrote npm package paths inside the bundle; package code holds no runtime
  strings it touches). Both after one `npm install` in the migrated project.
- **What real projects needed** (all reported in `MIGRATION.md`): ModularMC entry order
  (concurrent `stop` entries first, as `MapTs.apply`), case-insensitive source lookup with a
  portability warning, `UNPACK:SUBFUNCTION` folders, the content-guide fields stripped like the
  filter does, `text_replacer` on fragments and script sources (import specifiers kept),
  subfunctions syntax left after ModularMC frozen from the oracle's expansion (Beacon does not
  implement the subfunctions language), data-dir `package.json` moved to the root with `file:`
  packages vendored under `vendor/`, `scriptModules` from the legacy manifest's `module_name`
  dependencies, `packMeta.packDependencies` from the legacy BP/RP dependency directions.
- **Build changes made for migration**: bundler externals are exactly `@minecraft/server` plus
  the `scriptModules` keys (npm-only `@minecraft/*` packages are bundled);
  `packMeta.packDependencies` (`bp-rp` default, `rp-bp`, `mutual`, 4.2.1).
- **Perf**: `Index::read` caches file contents (1 MiB per file, 32 MiB total, valid while the
  indexed hash matches; one-shot builds start with a fresh index) and watch no longer waits on
  in-place edits; macOS watch-JSON p95 had crossed its 100 ms budget.
- **Open**: The-Great-Adventure Education is the only Education project verified (5.9 risk
  stands); a legacy project with no BP/RP dependency gets Beacon's default BP->RP one.

### 14.4 Phase 4 as built (2026-10-19)

Step 0 (2026-10-19) fixed the contract; code is the source of truth:
`crates/engine/src/{symbols,api_index,debug,plugin}.rs`, `crates/serve/` (daemon, discovery,
shim, LSP, MCP tool table), `crates/api/` (API index builder), diagnostic codes
`BCN0140`-`BCN0180`. Decisions and deviations:

- **Symbol table (5.7)** is lexical, per file class (JSON/JSONC, `.lang`, mcfunction,
  script string literals): `serde_json` keeps no positions and ids hide in lang keys,
  Molang, commands and scripts (4.3). Locations are **byte ranges** (like `diag::Fix`);
  line/column and LSP UTF-16 positions are derived. Definitions come from known fields per
  file kind and from generator output (a range only where the id is one literal in the
  generator source, else not renameable). A lang key embedding an id (`entity.demo:car.name`)
  is also a reference to that id, so renaming the entity renames the key. Phase-4 kinds:
  entity, item, block, lang key, function, animation, animation/render controller, texture,
  sound; loot/trade paths and families/tags later. `beacon rename <from> <to> [--kind]`
  (all-or-nothing, `RenameBlocked`), `beacon symbols [--find] --json` (`schema: 1`).
  Static outputs are indexed but never edited.
- **`beacon serve`**: new crate `crates/serve` (10). Handshake line (`Hello` / `HelloReply`)
  then the channel's own framing: LSP, MCP (the phase-2 newline JSON-RPC), control
  (`status`, `stop`). Pipe/socket names and discovery as in 3, with a random suffix on the
  Windows pipe name (squatting) and `File::try_lock` as the lifetime lock. Idle shutdown 60 s
  after the last client. Trust per connection from the stored decision or the shim's
  `--trust`; MCP write tools need the shim's `--allow edit|build|release|deploy`. One engine
  thread owns sessions, symbols, document overlays and the API index. The CLI's warm path
  through the daemon (3) and the TCP endpoint wait for phase 5 (the app). `beacon mcp`
  becomes `beacon serve --stdio --mcp`.
- **MCP grants and guardrail (8.1, 8.2).** Write tools (`rename_symbol`, `fix`, `add_content`, `build`, `deploy`, `pack`) take `dryRun` (default true, a read: no grant); `dryRun: false` needs the shim's `--allow edit|build|release|deploy` and otherwise fails with `GrantRequired` naming the flag. Trust and grants are per connection, never from tool arguments. Because `--trust` and `--allow` live in the agent's own MCP config, `beacon hook pre-tool-use` also denies edits to `.mcp.json`, `.claude/settings.json`, `.claude/settings.local.json` and `.vscode/mcp.json` (project-local, so an agent cannot grant itself access). This protects the config path only: an agent with arbitrary shell access, or a user-level config, is out of scope. `build` / `deploy` / `pack` with `dryRun` validate (as `check`) and write nothing; the emitted-file diff waits for engine support.
- **What `--trust` on an agent connection permits.** It trusts the project as it was at connect: generators and `beacon.plugins` entries (external processes such as `legacy_filter`) run in that connection's `check`, `build` and LSP features with no further grant. It is pinned to the `beacon.plugins` at connect: the engine thread revalidates before every job, and a connection whose `beacon.plugins` no longer match (an agent editing `config.json`), like one trusted by the stored decision whose key no longer matches, is demoted to restricted until it reconnects. `--trust` does not grant writes (`--allow`) and does not cover generator *sources* edited later: those run with the trust the connection holds, as in `beacon watch`.
- **LSP crate: `lsp-server` + `lsp-types`**, not `tower-lsp` (supersedes 9.1): a minimal
  stdio server measured 1.0 MB vs 2.8 MB with `tower-lsp-server` + tokio (Windows,
  release, stripped), no async runtime in a resident process, and the codebase is
  std-thread throughout. Subset: push diagnostics, definition, references, rename with
  prepare, completion, hover; full-document sync.
- **API index sources are MIT npm packages** (`@minecraft/server`, `server-ui`, ...,
  `@minecraft/vanilla-data`, `@minecraft/bedrock-schemas`: `"license": "MIT"` in each
  tarball's `package.json`, checked at build; only `bedrock-schemas` ships a `LICENSE` text,
  (c) Microsoft, so the 13 checklist item for `@minecraft/*` stays open). bedrock-samples
  `metadata/` (EULA) is not used in phase 4, so the 15 licensing risk no longer blocks the
  index; commands and Molang grammars wait for the legal review. Tarballs are hash-pinned
  per supported game version in `beacon-api`, the derived index is cached at
  `<data dir>/beacon/api/<game version>/index.json`, and a missing index is a note
  (`ApiIndexMissing`), never an error. Component checks use names from the schemas, not
  full JSON Schema validation. Script checks are named imports and manifest module versions;
  member-level checks stay with `check --full` (tsgo).
- **Debugger (6.6), verified** against Mojang/minecraft-debugger `c54af36` (2026-10-13) and
  BDS 1.26.52.3: launch fields `type: minecraft-js`, `request: attach`,
  `mode: connect|listen`, `localRoot`, `sourceMapRoot`, `generatedSourceRoot`, `host`,
  `port` (19144), `targetModuleUuid`, `passcode`. BDS `server.properties` has
  `allow-inbound-script-debugging`, `force-inbound-debug-port`,
  `script-debugger-auto-attach=listen`, `script-debugger-auto-attach-timeout`,
  `script-debugger-listen-address`, `script-debugger-passcode`. Today's bundle maps cannot
  bind breakpoints (esbuild writes `sources` relative to its temp outdir, or `file:///D:/`
  across drives); dev-like builds add a **debug mirror** `.beacon/debug/<target>/<output>/`
  with project-relative `sources`, `file` relative to the map's own folder, and a copy of the
  bundle beside the map. The game names a script relative to the pack's `scripts/` folder
  (`main.js`), so `generatedSourceRoot` is the mirror's `scripts` folder, while `localRoot` and
  `sourceMapRoot` stay at the mirror root (proven by the e2e below). Dev `targetModuleUuid`s are per worktree, so committed
  `launch.json` entries carry only `beaconOutput` / `beaconTarget`, and the VS Code extension
  resolves them at launch (a debug configuration provider for `minecraft-js`);
  `beacon debug-config --json` prints the resolved config for other editors. Beacon never
  speaks the wire protocol (backlog B6).
- **VS Code extension** lives in `integrations/vscode/` (next to `integrations/claude-code`;
  10 said `apps/vscode`). It launches `beacon serve --stdio --lsp` from PATH (or a setting),
  and contributes the LSP client, a trust prompt (`beacon trust --yes`), build/watch/deploy
  commands, the `minecraft-js` debug configuration provider, and the official debugger as a
  recommended extension. Previews and the content log channel wait for phase 5.
- **ctxtruth-bedrock** lives in `integrations/ctxtruth-bedrock/` (npm package; 10 said
  `packages/`), as specified in 8.4b; its Beacon-side inputs are `--help --json`,
  `symbols --json` and `api --json` (`schema: 1` each).
- **`beacon plugin update`** pins git-sourced plugins. The only runnable plugin kind is still
  `legacyFilter`, which gains `id` + `source: {git, ref}`. The lockfile gets `plugins.<id>`
  (`git`, full `commit`, blake3 tree `hash`; absent when empty, so existing lockfiles stay
  byte-identical). Builds use pins only. Update resolves with the system `git`, shows the
  diff, and writes on confirmation. The trust key becomes the plugin array plus the pins (only
  when pins exist), so an update asks for trust again. JS transform plugins (5.10) are not in
  phase 4. As built (`crates/engine/src/plugin.rs`, `crates/cli/src/plugin.rs`): `id` and
  `source` sit inside the `legacyFilter` object; `source.git` is `https://` (`file://` is
  accepted for local mirrors and tests); `cwd` is then relative to the checkout. Checkouts live
  at `<data dir>/beacon/plugins/<id>/<commit>/` (`BEACON_HOME` replaces `<data dir>/beacon`),
  without `.git`, files read-only; every build re-hashes the checkout (sorted paths + per-file
  blake3, modes ignored, links refused) against the pin: no pin, a pin whose git URL differs
  from `config.json`, or no checkout = `PluginUnpinned`; a different hash =
  `PluginHashMismatch`, and nothing runs. Builds never touch the network, so a fresh clone of a
  project runs `beacon plugin update <id>` once (it restores the checkout when `ref` still
  resolves to the pinned commit). `update [<id>] [--yes] [--json]`: `git ls-remote`
  (tag peeled, then branch; a full SHA is taken as is), then `init --template=` + `fetch
  --depth 1 <sha>` + `checkout` in a scratch dir, always with `core.hooksPath` pointing at an
  empty dir (no cloned or templated hook runs), `core.symlinks=false`, `autocrlf=false`, no
  shell, no prompts. The diff (old to new commit, added / removed / changed files; against the
  old checkout when it is still on disk) is printed; the lock is written after `--yes` or a
  TTY `y`; without either the exit code is 2 and nothing is written. It runs in restricted
  mode (it runs `git`, not project code). The trust key is `plugin::project_trust_key`
  (`config.json` plugins, plus `{"pins": ...}` when the lock has pins) and is used by the CLI
  and the serve daemon; a project without pins hashes exactly as before.
- **`beacon api`**: `install [--game <ver>]` (consent as for esbuild: `--yes`,
  `BEACON_DOWNLOAD=yes` or a TTY), `status`, `lookup <query>`, and bare `beacon api --json`,
  the ctxtruth-bedrock input, `schema: 1`: `{"schema": 1, "gameVersion": "1.26.50" | null,
  "installed": bool, "modules": {"@minecraft/server": ["2.10.0", "2.11.0-beta"], ...},
  "vanilla": {"entity": 129, "item": 1623, ...}}` (`modules` / `vanilla` are empty when not
  installed; exit 0). `--game` defaults to the newest pinned version not above the project's
  highest `targetVersion`, else the nearest one above. `check` resolves the installed index the
  same way (`api_index::resolve`) and says which one it used; an index older than the target
  skips the Error checks (module versions, named imports), whose APIs it may predate. `status --json`: `{"schema": 1, "versions": [{"gameVersion",
  "installed", "path"}]}`; `lookup --json`: `{"schema": 1, "gameVersion", "hits": [{"path",
  "signature", "doc", "deprecated"}]}`.
- **Exit verification (as built)**:
  1. *Rename*: machine-verified. `fixtures/rename` -> `fixtures/rename.expected` byte-identical
     through `beacon rename` and through the LSP (`crates/serve/tests/lsp.rs`), then a clean
     `check`.
  2. *Breakpoints in TS source*: the official adapter (`session.ts` of Mojang's
     minecraft-debugger, MIT, full-SHA pin, built unmodified with a `vscode` stub, run out of
     process) driven by a Node DAP client against BDS 1.26.52.3 in auto-attach `listen` mode
     reached `verified` and `stopped` at the breakpoint's TS line, locally on Windows
     (`integrations/vscode/test/e2e/run.mjs`; `.github/workflows/vscode-e2e.yml` runs it with
     `continue-on-error` until green on CI). Not machine-verified, **pending one human run**:
     F5 inside real VS Code with the Beacon and official extensions on a `beaconOutput` /
     `beaconTarget` launch entry (the two debug-configuration providers' interaction).
  3. *Serve idle budget*: machine-verified in CI (`idle_rss_serve_mb`, phase 4: daemon found
     via `--status` + shim, after an MCP `check` on the large corpus; 27 MB measured against
     100 MB).
- **Production gate**: with the 1.26.50 index installed, `beacon check` on the migrated
  Dimension-Clash and The-Great-Adventure (both editions) runs the API checks with no
  BCN0151-0155 findings; it reports three genuine BCN0140 dangling `function` calls in
  The-Great-Adventure.
- **Watch latency**: route memo keyed on config + paths + `module.json` hashes, `children`
  rebuilt only on key-set changes, merge by sorted runs, ledger written without read-back,
  symbols memo; local JSON p95 60.6 -> 36 ms (macOS CI had reached 95.5 ms of 100).

### 14.5 Phase 5 (in progress)

Roadmap row: desktop app (Play, visual settings and `module.json` forms, trust prompt) + setup
wizard + `beacon test` on BDS + signing/distribution + updater. Exit criteria (both need a
human, both **OPEN**): (1) clean Windows machine: install -> Play in under 5 minutes, no other
installs; (2) a non-coder completes it unaided.

Binding decisions for this phase: release artifacts are **unsigned** and **nothing is
published** (no GitHub release, no store, no live update feed); signing is a hook that is off.
The BDS EULA is accepted (download pinned, acceptance logged, never redistributed). Education
is verified only on The-Great-Adventure.

**Desktop app (6.1, 11.2).**

- **Tauri 2** + React (Vite, pnpm), as 6.1 and 11.2 say. Against the 12 budgets: Tauri uses
  the system WebView2, so the app binary is a few MB (Electron ships Chromium, ~150 MB on
  disk and ~200 MB idle); the installer carries the app + the `beacon` CLI (7.5 MB measured,
  14.1) + the WebView2 bootstrapper (~2 MB), and downloads nothing else at install. Idle RAM
  is the risk: the WebView2 process tree is ~80-110 MB, plus the project daemon (27 MB
  measured, 14.4) when a project is open, against **App idle RAM < 150 MB** (sum over the
  process tree, 12). Measured in wave 3 (`app_idle_rss_mb`); Monaco and three.js previews are
  not in the alpha (they are the largest idle-RAM items).
- **Layout**: `apps/desktop/src-tauri` (Rust shell) and `apps/desktop/ui` (React), as 10.
  The shell crate has its own `[workspace]` table and `Cargo.lock`: it is **not** a member of
  the root workspace. Tauri adds ~400 crates and WebView/WebKit system deps; the root
  `cargo test --workspace` and the phase 1-4 CI stay as they are. The logic is not in the
  shell: every `#[tauri::command]` forwards to one method of `beacon_serve::app::AppCore`
  (workspace member, tested without a webview), so the excluded crate is glue only. A
  dedicated workflow builds it.
- **Engine access: the app is a daemon client**, not an in-process engine. The tiebreaker is
  the single-writer model (3): with VS Code or an agent attached to the project's daemon, an
  app linking the engine would be a second writer of `config.json` / `module.json`. So
  project operations (check, Play's build + deploy, settings writes) go to the project's
  daemon over the owner-only pipe, through `shim::connect` (same discovery, version restart
  and lifetime lock as every other client), on the MCP channel with grants
  `edit,build,deploy` (never `release`). Things that are not per-project daemon state run in
  process: the trust store (`engine::trust`), `init` (`engine::init`). `setup` and `test`
  run the bundled `beacon` binary with `--json` (one implementation for CLI, VS Code and app;
  long downloads and BDS runs are a cancellable child process).
- **No TCP endpoint** (deviation from 3 and 14.4): the webview never talks to the daemon;
  Tauri IPC into `AppCore` replaces the planned TCP + bearer-token + Host/Origin endpoint,
  which is therefore not built. Smaller attack surface (no DNS rebinding, no token in the
  webview).
- **Trust (5.5)**: the app never sends `trust: true` in its `Hello`; connections are trusted
  only by the stored decision. The prompt (`trust_prompt`) lists what will run (generators,
  script bundling, each plugin, "runs arbitrary code" for external processes) and carries the
  trust key it was built from; `trust_grant(root, key)` records trust only if the key still
  matches, else `DocumentStale` and the prompt is shown again (no TOCTOU between what the
  user read and what is trusted). Projects without code need no prompt (14.1).
- **IPC command set** (`app::COMMANDS`): `app_info`, `setup_status`, `setup_run`,
  `project_open`, `trust_prompt`, `trust_grant`, `init_questions`, `init_create`, `check`,
  `play`, `settings_read`, `settings_write`, `test_run`, `update_check`, `update_install`,
  `update_rollback`. Events: `beacon://diagnostics`, `beacon://content-log`,
  `beacon://progress`. Errors cross IPC as `{code: "BCN....", message}`. Tauri 2 capabilities
  allow exactly these commands; no shell, fs or http plugin is exposed to the webview.
- **Untrusted text** (content log, project-derived diagnostics, plugin output) reaches the
  webview only through `app::sanitize` (ANSI and control characters stripped, 16 KiB cap) and
  is rendered as React text, never as HTML.
- **Play** = `build` + `deploy` of the first `dev`-artifact target through the daemon, then the
  game is launched by URI (`minecraft://` Bedrock, `minecraft-preview://`,
  `minecraftedu://`; to verify per install). Diagnostics stream in; the content log is tailed
  from the detected install. Play does not open a specific world (no verified URI for it); the
  managed dev world (4.3) is in the game's world list.
- **Visual settings and `module.json` forms**: rendered from Beacon's own JSON Schemas for
  `module.json` and the `beacon` key of `config.json` (embedded in the engine, also usable as
  `$schema` in editors). A small own form renderer (the field kinds are few: string, enum,
  bool, string maps, priority maps, the ordered outputs list with drag-to-reorder), no form
  framework. `settings_read` returns value + schema + `base` (blake3 of the bytes);
  `settings_write` validates with the same parsers the build uses (`ModuleJson`,
  `ProjectConfig`) and writes through a new daemon MCP write tool (`write_settings`, grant
  `edit`): stale base = `DocumentStale` (new, `BCN0164`), dirty editor buffer =
  `DocumentDirty`. The writer edits only the changed values in the existing text (key order,
  comments and formatting of untouched parts kept); it never re-serializes the whole file.

**Setup wizard (9).** `beacon setup [--json] [--dry-run] [--yes] [--accept-eula] [<tool>...]`,
rendered by the app and the VS Code extension from `engine::setup::SetupReport`
(`schema: 1`):

- Downloads exactly three things, each on demand, each hash-pinned in the binary: **esbuild**
  (`crates/bundler` pin, pre-selected), the **API index** (`crates/api` pins, pre-selected),
  **BDS** (`crates/bds` pin, never pre-selected, only for `beacon test`, only after EULA
  acceptance). Nothing else is installed: no Node, no Python, no VS Code. Defaults
  pre-selected, one "Continue".
- **Minecraft detection** (`setup::detect`): Bedrock GDK (`%APPDATA%\Minecraft
  Bedrock\Users\Shared\games\com.mojang`) before UWP (`%LOCALAPPDATA%\Packages\
  Microsoft.MinecraftUWP_8wekyb3d8bbwe\LocalState\games\com.mojang`; a machine that moved to
  GDK keeps a stale UWP folder), Preview GDK / UWP (`Minecraft Bedrock Preview`,
  `Microsoft.MinecraftWindowsBeta_8wekyb3d8bbwe`), Education (`%APPDATA%\Minecraft Education
  Edition\games\com.mojang`). Dev deploy uses `setup::preferred`: the first stable install of
  the edition, never Preview implicitly (`artifact::com_mojang` now delegates to it; behavior
  unchanged). `BEACON_COM_MOJANG` still overrides. No install found = `MinecraftNotFound`
  (`BCN0200`, a warning: build works, Play cannot deploy).
- `beacon` on PATH is done by the installer (per-user PATH), not by the wizard.
- **Deviation from 9 step 2**: the VS Code / Cursor / Windsurf extension and MCP registration
  offers move out of the first-run wizard into Settings > Integrations (after the first Play),
  to keep the 5-minute path to one screen.
- `beacon doctor` re-runs detection and pin checks and reports drift with the same shape.

**`beacon test` on BDS (8.5).** Contract `engine::gametest`:

- `beacon test [--target t] [--filter glob] [--timeout s] [--accept-eula] [--yes] [--json |
  --sarif]`: builds the target as an `.mcworld` with dev UUIDs and the Beta APIs experiment on,
  adds a Beacon runner behavior pack, runs it on the pinned BDS (`crates/bds`, extracted into a
  library: pin, EULA gate + log, member-confined extraction, `server.properties`, process
  guard), sends `gametest runall`, and parses the console.
- The runner protocol is Beacon's own: one console line per event, `BEACON_TEST {"event":
  "start"|"pass"|"fail"|"done", ...}`, so results never depend on BDS or GameTest wording.
  Console text is untrusted: malformed lines are ignored. A test that started and never
  reported is `timedOut`; no `done` = the run timed out.
- Output: `TestReport` (`schema: 1`: `bdsVersion`, `results[{name, status, message}]`,
  `complete`) with `--json`; SARIF with `--sarif` (`GameTestFailed` `BCN0190`,
  `GameTestTimeout` `BCN0191`, `GameTestNone` `BCN0192`); `BdsEulaRequired` `BCN0193`,
  `BdsFailed` `BCN0194`. Exit 0 all passed, 1 otherwise.
- How the runner observes results through `@minecraft/server-gametest` on BDS 1.26.52.3 is
  **unverified**; wave 1 starts with a spike against real BDS (as the debugger was in 14.4).

**Distribution (9, 9.2).**

- **Windows**: Tauri bundler **NSIS**, `installMode: "currentUser"` (per-user, no admin, no
  UAC prompt), WebView2 `embedBootstrapper` (Windows 11 ships WebView2; the bootstrapper
  covers Windows 10 machines without it; the fully offline installer variant is later). The
  `beacon` CLI is an `externalBin` sidecar; an NSIS installer hook adds the install folder to
  the per-user PATH and removes it on uninstall. MSI is not used (per-machine, needs admin).
- **macOS**: `.dmg`, unsigned with an ad-hoc signature (needed for Apple silicon to launch).
  **Linux**: AppImage built, not tested, not release-gated (6.1).
- **Unsigned pipeline**: `.github/workflows/desktop.yml` builds Windows and macOS on every push
  and uploads the installers as **workflow artifacts only**: no GitHub release, no winget/brew
  manifest, no store.
- **Signing hook (off)**: the workflow merges `apps/desktop/src-tauri/tauri.sign.conf.json`
  (Windows `signCommand`) only when the repository variable `BEACON_SIGN` is `1` and the
  secrets exist; macOS signing/notarization switch on only when `APPLE_*` secrets exist.
  Neither exists, so every artifact is unsigned. Consequence for the exit test: Windows
  SmartScreen shows "Windows protected your PC" (More info -> Run anyway), macOS Gatekeeper
  blocks the first open; both are steps in the human checklist and count toward the 5 minutes.
- 9.2 "releases are signed" and "code signing ... prerequisites of the alpha" are deferred by
  the user's decision; recorded as a deviation.

**Updater (9).** Contract `serve::update`:

- `tauri-plugin-updater` with Tauri's static feed JSON (`version`, `notes`, `pub_date`,
  `platforms.<os-arch>.{url, signature}`); the plugin verifies the downloaded installer's
  **minisign** signature against the public key compiled into the build before running it
  (minisign as 9.2 names it). The key is generated by the maintainer later
  (`tauri signer generate`); the private key never enters the repo.
- **Fails closed**: `update::BUILD` takes the key and the feed from the release environment
  (`BEACON_UPDATE_PUBKEY`, `BEACON_UPDATE_FEED`); both are unset, so every build is
  `Decision::Disabled` (`UpdateDisabled`, `BCN0211`) and never fetches. A malformed feed, a
  non-`https` URL, a missing signature, or an unparsable version is `UpdateRejected`
  (`BCN0210`); the plugin rejects a bad signature.
- **Consent**: `update_check` fetches and evaluates only (no identifiers in the request);
  `update_install` runs only on the user's click. No background install. Check-on-launch is
  off in the alpha (there is no feed).
- **No downgrades** through the feed (not newer = up to date). **Rollback** is explicit: the
  previous installer is kept with its blake3 and signature (`update::Rollback`) and both are
  re-verified before it runs. **Channels** are separate feeds: `<feed>/{stable,beta}/latest.json`.
- Not live: no key, no feed, nothing published.

**Diagnostic codes reserved**: `DocumentStale` `BCN0164`, `GameTestFailed` `BCN0190`,
`GameTestTimeout` `BCN0191`, `GameTestNone` `BCN0192`, `BdsEulaRequired` `BCN0193`,
`BdsFailed` `BCN0194`, `MinecraftNotFound` `BCN0200`, `UpdateRejected` `BCN0210`,
`UpdateDisabled` `BCN0211`.

**Budgets (12)**: two rows to add to `perf/budgets.json` (staged now; Step 0b):
`{"id": "app_idle_rss_mb", "label": "App idle RAM (app + WebView2 tree + daemon, project open)",
"budget": 150, "unit": "MB", "phase": 5, "tolerance_pct": 10}` and `{"id":
"installer_size_mb", "label": "Windows installer (app + beacon CLI + WebView2 bootstrapper)",
"budget": 25, "unit": "MB", "phase": 5, "tolerance_pct": 10}`. The CLI's 30 MB binary row is
unchanged; the app binary is covered by the installer row.

**Deviations, summarized**: no TCP endpoint (Tauri IPC); the app shell is outside the root
workspace; editor-extension and MCP offers leave the first-run wizard; WebView2 bootstrapper
instead of the offline installer; releases unsigned and unpublished (9.2 signing deferred);
updater present but disabled.

---

The wave plan, the machine assessment and the decisions taken after the plan review (they
override parts of this section: per-call daemon connections, grants `edit,deploy`, esbuild in
the installer, no PATH change, updater policy only, `module.json`-only forms) are in
[phase5-plan.md](phase5-plan.md).

---

## 15. Risks and open questions

- **Generator perf in QuickJS**: heavy one-to-many generators are interpreted. Mitigation:
  phase 0 spike, budgets on the corpus, V8 perf fallback behind the trait (11.2).
- **Migration coverage**: real `_map.ts` / `_map.py` files may use features the migrator
  cannot translate into generators. Mitigation: evaluated output kept as the oracle,
  untranslated parts emitted as generator skeletons with diagnostics, never silently dropped.
- **Format churn**: schemas and the API index must be updated every game release; automate
  diffing.
- **Education Edition specifics**: transforms in 5.9 come from one project
  (The-Great-Adventure); verify the Education-target direction (flags set to 1, experiments)
  on a second Education project before phase 2.
- **bedrock-samples license**: if a derived API index cannot ship, every user downloads the
  metadata on first `check`; acceptable, but it must be fast and cached per tag.
- **Windows realities**: files held open by the game, UWP vs GDK paths, long paths,
  case-insensitivity, junctions, AV scanning. Covered by the emit ledger, watch rules (9.3),
  and Windows CI.
- **Ecosystem loss**: Regolith community filters run only through `legacy_filter`.
- **Scope vs team**: phases 1-4 serve the team before the alpha; the backlog stays out until
  the alpha ships.
- **BDS in CI**: EULA acceptance flow to confirm.
- **Open**: rolldown adoption timing (changes content hashes); Molang tooling scope; whether
  skin packs need anything beyond the built-in.
