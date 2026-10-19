# Phase 5 (alpha) - Step 0: contract and plan

> Written 2026-10-19 while phase 4 wave 4 is staged for signing. `docs/ARCHITECTURE.md` is in
> that staged set, so the `### 14.5` section below lives here; Step 0b moves it into
> ARCHITECTURE.md after the phase-4 commit lands. Code is the source of truth for the
> contract: `crates/engine/src/{setup,gametest}.rs`, `crates/serve/src/{app,update}.rs` (+
> `app/`), diagnostic codes `BCN0164`, `BCN0190`-`BCN0211`.

---

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

## What Step 0 could not do (staged files) - Step 0b, after the phase-4 commit lands

The head does these inline (small, sequential) before wave 1:

1. `docs/ARCHITECTURE.md`: paste the 14.5 section above after 14.4; in 3 replace "TCP is used
   only where a pipe is impossible (the app's webview)" with a pointer to 14.5; in 6.2 nothing
   changes (`test`, `doctor`, `setup` are already phase 5).
2. `crates/cli/src/main.rs` + `crates/cli/Cargo.toml`: subcommands `Test`, `Setup`, `Doctor`
   wired to `todo!()` handlers in new files `crates/cli/src/test.rs` and
   `crates/cli/src/setup.rs` (so wave-1 briefs own disjoint files); `beacon-bds` as a
   dependency of `beacon-cli` (this changes `Cargo.lock`).
3. `crates/bds/src/lib.rs`: create it with `pub const VERSION`, the pins as `pub const WIN /
   LINUX: Pinned` and `pub fn pin() -> Option<Pinned>`, moved out of `main.rs` unchanged
   (`main.rs` uses them from the lib). The rest of the extraction is wave 1A.
4. `perf/budgets.json`: the two rows above.

---

## Wave plan

Every wave starts only after the previous one is merged and green. Wave 1 needs Step 0b,
which needs the signed phase-4 commit. Verify for every brief, besides its own commands:
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (with `BEACON_ESBUILD` set as in Step 0).

### Wave 1

**1A - `beacon test` on BDS** (model: **opus**; unverified upstream behavior, like 14.4's
debugger work, and a BDS process lifecycle)

- Goal: `beacon test` runs a project's GameTests on the pinned BDS and reports
  `TestReport` / SARIF as `engine::gametest` specifies.
- Files: `crates/bds/src/lib.rs`, `crates/bds/src/main.rs`, `crates/cli/src/test.rs`,
  `crates/engine/src/gametest.rs` (runner pack generation + Beta APIs toggle),
  `fixtures/gametest/` (one passing, one failing, one never-finishing test),
  `crates/cli/tests/test_cmd.rs`, `.github/workflows/gametest.yml` (new; windows-latest, BDS
  cache keyed by the pin hash, EULA env as in `bds-smoke`).
- Steps: spike first (does `gametest runall` run registered tests on BDS 1.26.52.3 with the
  Beta APIs experiment; can the runner wrap `register` to see pass/fail; record the answer in
  the brief's report); then lib extraction (`run(world, opts) -> console lines`, existing
  `cargo bds smoke` / `debug-listen` unchanged); then `beacon test`.
- Acceptance: on `fixtures/gametest`, `beacon test --json --accept-eula --yes` exits 1 with
  results `pass`, `failed` (message from the test), `timedOut`, and `--sarif` has `BCN0190` +
  `BCN0191`; `--filter` that matches only the passing test exits 0; without EULA acceptance
  exit 2 with `BCN0193` and no download; `cargo bds smoke` CI job still green.
- Verify: `cargo test -p beacon-engine gametest`, `cargo test -p beacon-cli --test test_cmd`
  (BDS part `#[ignore]`d unless `BEACON_BDS_EULA=accept`), the new workflow.

**1B - `beacon setup` / `beacon doctor`** (model: **sonnet**)

- Goal: the wizard's engine side: detection + three pinned, consented downloads, one JSON shape
  for CLI, VS Code and app.
- Files: `crates/cli/src/setup.rs`, `crates/cli/tests/setup.rs`, `crates/engine/src/setup.rs`
  (only if a field is missing).
- Acceptance: `beacon setup --json --dry-run` prints `SetupReport` (`schema: 1`) with the
  installs `setup::detect` finds and three steps with the pinned version and size, nothing
  downloaded; `beacon setup esbuild api-index --yes` installs exactly those (esbuild via
  `bundler::download::install`, index via the `beacon api install` path); `bds` without
  `--accept-eula` is `BCN0193`, never a download; a declined step is `declined`; `doctor`
  exits 1 when a pin check fails. With `BEACON_HOME` in a temp dir, no network in tests (pins
  pre-seeded the way `crates/bundler` tests do).
- Verify: `cargo test -p beacon-cli --test setup`.

**1C - Desktop shell + unsigned pipeline + signing hook** (model: **sonnet**)

- Goal: an installable, unsigned Tauri app that opens a project, shows the trust prompt,
  creates a project from the built-in template; artifacts from CI.
- Files: `apps/desktop/**` (src-tauri with its own `[workspace]`, `tauri.conf.json`,
  `tauri.sign.conf.json`, capabilities generated from `app::COMMANDS`, NSIS hook for the
  per-user PATH; ui with project browser, trust prompt, new-project form from
  `init_questions`), `crates/serve/src/app.rs` (`app_info`, `project_open`, `trust_prompt`,
  `trust_grant`, `init_create`), `crates/serve/tests/app.rs`, `.github/workflows/desktop.yml`
  (new).
- Acceptance: `trust_grant` with a stale key is `BCN0164` and records nothing; `init_create`
  makes a trusted project; capabilities allow exactly `COMMANDS`; `desktop.yml` produces an
  NSIS `.exe` (per-user) and a `.dmg` as workflow artifacts, with no release step; with
  `BEACON_SIGN` unset no signing step runs; locally `cargo tauri build` (after
  `cargo install tauri-cli --locked`) produces the installer and it installs without UAC.
  So that later briefs never share a file: the UI router already has a placeholder route per
  screen (`Play`, `Test`, `Settings`, `Setup`, `Home`, `About`) and `src-tauri/src/main.rs`
  already declares `mod updater; mod smoke;` (empty stubs) and registers every command.
- Verify: `cargo test -p beacon-serve --test app`; `pnpm -C apps/desktop/ui test`;
  `cargo tauri build` in `apps/desktop`; CI run of `desktop.yml`.

### Wave 2

**2A - Play + content log** (model: **sonnet**)

- Goal: one button: build + deploy through the daemon, launch the game, stream diagnostics
  and the content log.
- Files: `crates/serve/src/app/play.rs` (`check`, `play`, `test_run`), `crates/serve/tests/
  app_play.rs`, `apps/desktop/ui/src/screens/Play.tsx`, `apps/desktop/ui/src/screens/Test.tsx`.
- Acceptance: with `BEACON_COM_MOJANG` at a temp dir, `play` on a fresh `init` project returns
  no errors and the dev packs are in that dir; the daemon was used (its status shows the
  client); untrusted project with code returns `BCN0050` without building; no install found
  is `BCN0200` in the report; content-log lines pass through `sanitize`; the launch URI per
  product is verified on this machine (GDK Bedrock + Education installed) and recorded.
- Verify: `cargo test -p beacon-serve --test app_play`; manual Play on this machine.

**2B - Settings and `module.json` forms** (model: **opus**; format-preserving JSON edits
against a versioned single-writer model)

- Goal: forms that write the same files, keeping the user's formatting.
- Files: `crates/engine/schemas/module.schema.json`, `crates/engine/schemas/beacon.schema.json`,
  `crates/engine/src/settings.rs` (schema access + minimal-text-edit writer),
  `crates/serve/src/mcp/` (new `write_settings` tool; grant `edit`; base hash; `DocumentDirty`
  on overlays), `crates/serve/src/app/settings.rs`, `apps/desktop/ui/src/screens/Settings.tsx`,
  `apps/desktop/ui/src/components/Form.tsx`.
- Acceptance: every `module.json` under `fixtures/` and `templates/` validates against the
  schema and parses with `ModuleJson` (both agree on a rejected sample too); changing one value
  leaves every other byte of the file identical (comments, key order, indentation); reordering
  outputs rewrites only the `outputs` array; stale base = `BCN0164`; dirty LSP buffer =
  `BCN0163`; invalid value = `BCN0002` / `BCN0003` and nothing written.
- Verify: `cargo test -p beacon-engine settings`, `cargo test -p beacon-serve --test mcp`,
  `pnpm -C apps/desktop/ui test`.

**2C - Setup wizard UI + first run** (model: **sonnet**)

- Goal: first launch shows the wizard (detected installs, three steps, one Continue), then
  "Create project" / "Open project".
- Files: `crates/serve/src/app/setup.rs`, `apps/desktop/ui/src/screens/Setup.tsx`,
  `apps/desktop/ui/src/screens/Home.tsx`.
- Acceptance: `setup_status` never downloads; `setup_run` downloads only the accepted tools;
  BDS is unchecked by default and needs the EULA checkbox; progress events arrive; declining
  everything still reaches Home; no install found shows the `BCN0200` message with the fix.
- Verify: `cargo test -p beacon-serve --test app` (setup cases, sidecar stubbed by a fake
  `beacon` like `integrations/ctxtruth-bedrock/test/fake-beacon.mjs`); `pnpm -C
  apps/desktop/ui test`.

### Wave 3

**3A - Updater (disabled build, tested end to end with a throwaway key)** (model: **sonnet**)

- Files: `crates/serve/src/app/updater.rs`, `crates/serve/src/update.rs`,
  `apps/desktop/src-tauri/src/updater.rs`, `apps/desktop/ui/src/screens/About.tsx`.
- Acceptance: the shipped build shows "updates are off in this build" and never fetches; a test
  build with a key generated in the test (never committed) and a local feed installs a newer
  signed installer, rejects a tampered one (`BCN0210`), refuses a downgrade, and rolls back
  only after re-verifying the kept installer's blake3 and signature.
- Verify: `cargo test -p beacon-serve update`; the src-tauri updater test.

**3B - Install smoke + timings + app budgets** (model: **sonnet**)

- Goal: the automatable half of exit criterion 1, on a clean VM.
- Files: `apps/desktop/src-tauri/src/smoke.rs` (`--smoke <dir>`: create the template
  project, Play into `BEACON_COM_MOJANG`, write timings JSON, exit), `scripts/install-smoke.ps1`
  (silent per-user install `/S`, first launch, smoke, uninstall; every step timestamped),
  `scripts/sandbox/install-smoke.wsb` (Windows Sandbox config mapping the installer and the
  script), `.github/workflows/install-smoke.yml` (new; fresh `windows-latest`, installer from
  `desktop.yml`'s artifact), `crates/perf/src/app.rs` (`app_idle_rss_mb`,
  `installer_size_mb`).
- Acceptance: the workflow reports install, first launch, setup downloads (esbuild + index)
  and Play-ready times and fails above 5 minutes total; no Node/Python on the runner's PATH
  for the smoke (PATH scrubbed); both new budget rows reported and enforced.
- Verify: the workflow on CI; `scripts/install-smoke.ps1 -Installer <exe>` in Windows Sandbox.

**3C - Human test kit** (model: **haiku**)

- Files: `docs/alpha-test.md` (non-coder script: one page, numbered steps with screenshots
  placeholders, a stopwatch table per step, "where did you get stuck" questions, the
  SmartScreen step spelled out), `README.md` install section, `docs/phase5-plan.md` ->
  14.5 as-built notes in ARCHITECTURE.md.
- Acceptance: every step in the script maps to a screen that exists; timings table matches the
  smoke's step names so the two can be compared.

### Needs a human (OPEN until done)

- **Exit 1** (clean Windows, install -> Play < 5 minutes, no other installs): needs a real
  clean Windows with Minecraft installed and signed in (CI and Windows Sandbox have no game, no
  Microsoft account). 3B times everything up to "packs deployed, game launch requested"; the
  human times the rest with `docs/alpha-test.md`'s table. Easiest setup: a fresh Windows 11 VM
  or a spare machine, Minecraft from the Store, the installer from `desktop.yml`'s artifact.
- **Exit 2** (a non-coder completes it unaided): one or more people from 2.3's persona follow
  `docs/alpha-test.md` with no help; the observer fills the stuck-points column. Unsigned
  SmartScreen / Gatekeeper prompts are part of it.
- Play's launch URI per product (2A) and the forms' look and feel (2B) need eyes once.

### What this machine can verify (checked 2026-10-19)

- Available: Windows 11 Pro 10.0.26300, MSVC (VS 2026 Community + Build Tools 2022), WebView2
  runtime 155, Node 22 + pnpm 10, Rust 1.95 (pinned), Minecraft Bedrock (GDK, plus a stale UWP
  folder) and Education installed, `WindowsSandbox.exe` present (feature state not readable
  without elevation).
- Checked in Step 0: Play's engine path needs no Node. `beacon init proj --yes` then
  `beacon build --target dev` with `PATH` reduced to `C:\Windows;C:\Windows\system32`, no
  `node_modules`, `BEACON_ESBUILD` pointing at a plain esbuild binary (standing in for the
  wizard's pinned download) and `BEACON_COM_MOJANG` at a temp dir: exit 0, dev BP/RP deployed
  (`@minecraft/server` is a bundler external, so the template's `package.json` is never needed
  to build).
- Missing: `cargo tauri` (`cargo install tauri-cli --locked` or `@tauri-apps/cli` via pnpm);
  NSIS is fetched by the Tauri bundler itself.
- Not verifiable here: "clean machine" and "no other installs" (this machine has Node, Python,
  VS Code, WebView2): needs Windows Sandbox (no game) or CI `windows-latest` (no game) for the
  install half, and a clean VM with the game for the full run; macOS dmg needs the CI macOS
  runner.

---

## Open questions and risks

- **App idle RAM**: WebView2 tree + daemon is close to 150 MB. If over: release the daemon when
  no project is open, keep the UI lean (no Monaco in the alpha), measure before adding views.
- **Unsigned installers vs a non-coder**: SmartScreen "Run anyway" is a known drop-off point;
  exit 2 may fail on it alone. Signing is the fix, deferred by decision.
- **GameTest result capture on BDS** is unverified (wave 1A spike); fallback is parsing the
  game's own GameTest console messages, with the same `TestReport`.
- **Launch URIs** and **content-log locations** for GDK Bedrock and Education are unverified.
- **Format-preserving writes** of `config.json` (possibly JSONC with comments) are the hardest
  part of the forms; if they slip, the forms can ship for `module.json` first.
- **Esbuild on the 5-minute path**: the minimal template has a script, so the first Play needs
  the esbuild download (~10 MB) from the wizard; slow networks eat into the 5 minutes.
- **WebView2 missing** on Windows 10 without internet: the bootstrapper needs the network;
  the offline installer variant is later.
- **Minecraft not installed** on the test machine is not something Beacon can install; the
  wizard reports it (`BCN0200`).
- **Education** stays verified only on The-Great-Adventure (5.9 risk unchanged).
- **BDS EULA in CI** stays as phase 2 records it (acceptance logged per run).
- Open: keep the app shell outside the root workspace for good, or join it once Tauri's
  dependency tree is acceptable for the main CI?

## Decisions after Fable ADVISE (2026-10-19) - these override the wave plan above

1. **Per-call daemon connections.** `AppCore` opens a fresh `shim::connect` per action, carrying
   only that action's grant (Play -> `deploy`, settings -> `edit`). A trust grant therefore takes
   effect on the next click (the daemon never re-promotes a connection, `daemon.rs`), and the
   daemon idles out when the user is not clicking. `ShimOptions.daemon` is
   `[beacon_exe, "serve", "--project", root]`, never `current_exe` (the Tauri binary).
2. **App grants are `edit,deploy`.** Play uses the MCP `deploy` tool (the `build` tool refuses dev
   targets). Never `build`/`release`.
3. **Trust prompt carries the plugin list it showed**; `trust_grant` records exactly that list via
   `trust::trust`, so the store's own hash check fails closed if the project changed. No second
   read, no separate compare.
4. **All project-derived text is sanitized and capped in `AppCore`** before it is returned
   (diagnostic messages, `TrustPrompt.runs/plugins`, content log), with a test.
5. **No PATH modification in the installer for the alpha.** Nothing on the exit path needs
   `beacon` on PATH. A reversible Settings > Integrations action may add it later, in Rust.
6. **esbuild ships in the installer** (second externalBin, copied into the tools dir on first
   launch, found by the existing hash check). The 5-minute path has zero downloads; the wizard is
   "detect Minecraft -> Continue". API index and BDS stay optional downloads in Settings.
7. **Updater = policy only for the alpha**: the tested fail-closed `update.rs` plus "updates are
   off" in About. No `tauri-plugin-updater` registration, no rollback, no channels, no e2e.
8. **Forms = `module.json` only** (no comments; re-serialized with key order preserved). 2B drops
   from opus to sonnet. `config.json` forms after the alpha.
9. **Minecraft never launched = BCN0200.** The fix text and `alpha-test.md` step 0 say: start
   Minecraft once, sign in, reach the main menu. Play shows it as the headline.
10. **"Clean Windows" means Windows 11** (WebView2 preinstalled). Windows 10 is non-gating.
11. **Installer delivery for testers** is fixed in 3C (a plain download with Mark-of-the-Web, so
    the SmartScreen step is the real one), and the kit describes exactly that path.
12. **Defender scan** of the installer and the `beacon.exe` sidecar is part of 1C's acceptance.
13. **Waves reordered to the critical path**: W1 = 1B (setup/doctor) + 1C (shell, pipeline, and
    the daemon-client helper `crates/serve/src/app/client.rs`); W2 = 2A (Play) + 2C (wizard);
    W3 = 3B (install smoke + timings + app budgets) + 3C (human kit); W4 = 1A (`beacon test`,
    off the exit path) + 2B (forms) + 3A (updater policy UI).
14. **`beacon test` spike starts with a bundler alias** of `@minecraft/server-gametest` to a
    runner shim inside the user's BP (a separate runner pack cannot see the user pack's
    registrations); the game's own "[GameTest] passed/failed" console lines are the fallback.
15. **Measure the empty 1C shell's idle RAM in wave 1.** If the bare shell is already near
    120 MB, the budget is revisited then.
16. Linux AppImage out of CI for the alpha; the macOS dmg builds but never gates.

## Play: launch methods (verified)

Verified 2026-10-19 on this machine (Windows 11; Bedrock GDK `Microsoft.MinecraftUWP_1.26.5020.0`
plus Education installed) by `Start-Process '<uri>'`, then reading back `Get-Process
Minecraft.Windows` (image path) and a fresh `Debug_Log*` in the game's log folder; also by a
real `AppCore::play` run against a scratch project (packs landed in the real com.mojang,
`launched: true`, game started; the scratch packs were removed afterwards).

- **Bedrock GDK**: `minecraft:` (registered `URL:minecraft`) starts `...\WindowsApps\Microsoft.MinecraftUWP_...\Minecraft.Windows.exe`. Verified.
- **Education**: `minecraftedu:` (`HKCR\minecraftedu\shell\open\command` -> `Program Files\Microsoft Studios\Minecraft Education\Minecraft.Windows.exe "%1"`) starts Education. Verified.
- **Bedrock UWP layout, Preview (GDK/UWP)**: **not verified** (no Preview here, `minecraft-preview:` is not registered; UWP layout is a stale folder). Play deploys and reports `launched: false`; no URI is guessed.
- Never launched when `BEACON_COM_MOJANG` overrides the folder.

Content log (verified): `ContentLog<date>_1.txt`, one per game start, lines `HH:MM:SS[Category][level]-text`
(levels verbose/inform/warning/error), in `%APPDATA%\Minecraft Bedrock\logs\` (GDK) and
`%APPDATA%\Minecraft Education Edition\logs\` (Education). The file is only written when
Settings > Creator > "Enable Content Log File" is on: with it off, both games wrote only
`Debug_Log*.txt` on today's launches. UWP/Preview folders unverified: not tailed. The shell must
call `AppCore::content_log_tail` after `play` (see `crates/serve/src/app/play.rs`).

## 3C notes (2026-10-19)

- `docs/alpha-test.md`, the README install section and ARCHITECTURE 14.5 (as built) done.
- Mismatches found: (a) the template prints `<namespace> loaded` (e.g. `my_addon loaded`), not
  "Beacon minimal loaded"; (b) the template's `dev` target has no `world`, so no managed dev
  world is made and the tester must create a world and activate the dev packs by hand; (c)
  `init_create` makes a trusted project, so the trust prompt is not shown in the happy path;
  (d) Setup.tsx preselects the API index (`defaultSelected`) inside the collapsed "Optional
  tools", so Continue downloads it, against decision 6 (optional, Settings); (e) the plan's
  14.5 text on trust (`DocumentStale`), grants, PATH, updater, esbuild was stale: fixed in
  ARCHITECTURE only (the text above is the original contract).
