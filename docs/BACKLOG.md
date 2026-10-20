# Beacon - Backlog

> Moved out of ARCHITECTURE.md in v0.4 (2026-10-15). Nothing here is committed or scheduled.
> Each item is revisited after the alpha (ARCHITECTURE.md section 14) with usage evidence from
> the team and collaborators. Design notes are kept so the ideas are not lost; review notes from
> the v0.3 architecture review are attached where relevant.

---

## B1. Visual graph authoring

Gameplay logic as node graphs (blocks with typed ports, connections, subgraphs), compiled to a
`@minecraft/server` TS runtime plus mcfunction. For designers and map makers; editable by agents.

| Package (TypeScript) | Role |
|---|---|
| `@beaconmc/graph-model` | Graph format (published spec), block registry, port compatibility |
| `@beaconmc/graph-runtime` | In-game signal dispatcher, bundled into the BP |
| `@beaconmc/graph-codegen` | `*.graph.json` -> TS + mcfunction; runs in the JS host as a generator |
| `@beaconmc/blocks-std` | Standard node library; blocks needing extra runtime libraries are optional packages |

- graph-model is bundled once and loaded in three places: JS host (build), daemon (MCP
  `graph.*` tools delegate to the host, JSON in/out), app (browser).
- Project metadata comes from `config.json`; `*.graph.json` holds only `graphs` and `definitions`.
- Graph `definitions` == `beacon add <kind>`; ports reference symbol kinds.

Review notes: a graph model, runtime, codegen, and block library are a product of their own.
The engine keeps the hooks it needs regardless (generators, single-writer document model,
symbol kinds), so adding graphs later does not require engine redesign.

## B2. Agent observability panel (ACP client + agent plugins)

A panel in the app (and a view in the VS Code extension) showing every AI agent working on
the project: sessions, subagent tree, status, plan, current tool call, files touched, and the
effect on `beacon check`.

```
AgentSession { id, agent (claude | codex | gemini | ...), origin, parentId?,
               status (running | awaiting-permission | idle | done | error),
               plan[], currentTool?, filesTouched[], diagnosticsDelta, usage? }
```

Sources: (1) ACP client for agents launched by Beacon; (2) Claude Code / Codex hook plugins
for agents started elsewhere (also the only reliable subagent-tree source; ACP has no
subagent concept); (3) attribution of calls to Beacon's own MCP tools.

Review notes: three integration surfaces tracking fast-moving external protocols, and a
forgeable event surface (hook events must be authenticated and sanitized before rendering).
Minimal v1 if revived: "files touched + `check` delta" from the file watcher plus MCP
attribution. Note: the Claude Code plugin as a **guardrail** (blocking PreToolUse hook) is
NOT in the backlog; it is in ARCHITECTURE.md 8.2.

## B3. Game capture for agents

Window-only capture of the Minecraft client (Windows Graphics Capture; ScreenCaptureKit on
macOS), low fps into an in-memory ring buffer, event-triggered frames (agent request,
content-log error, GameTest step, script `mark()`), token-economical delivery (change
detection, contact sheets, adaptive resolution, text-first pairing with inspector state,
WebP/JPEG). MCP tools `game.capture.now|watch|timeline|diff`.

Review notes, required if revived:
- Frames sent to an agent leave the machine (model provider): per-session consent, per-frame
  audit log entry, and a statement in the UI.
- Continuous capture (`watch`) only after human approval, never startable by an agent alone.
- OS-level indicator the agent cannot hide; rate and duration caps; optional HUD/chat redaction.
- In-game chat, coordinates, and other players' names are inside the window.
- ScreenCaptureKit permission is broader than one window on older macOS.
- Default to text state (inspector); pixels are the exception.

## B4. Game inspector

Live view of game state over the debug/script bridge (entities, scoreboards, dynamic
properties, tags). Prerequisite for the text-first half of B3.

## B5. Cinematic editor

Timeline of camera keyframes (position, rotation, easing, fades), text/sound cues, previewed
live in the game through the dev bridge, compiled to `/camera` commands (mcfunction) or a
script-API sequence. Agents can author it from a description since it is plain data. Requested
by the one user-research respondent (ARCHITECTURE.md 2.3).

## B6. Native script-debug bridge

`beacon serve` speaks the minecraft-debugger wire protocol: standalone DAP adapter for any DAP
client, MCP `debug.*` tools for agents, stats/profiler panels in the app, conformance tests
against BDS. Protocol is undocumented and versioned by the game; gate on `ProtocolVersion`.
Fork policy from ARCHITECTURE.md 6.5 applies.

## B7. Evals

Benchmark of authoring tasks graded on: diagnostics clean; static assertions on emitted packs;
GameTests on headless BDS. BDS has no client: it verifies behavior and scripts, never
rendering, models, or particles. BDS downloaded with explicit, logged EULA acceptance, never
redistributed.

## B8. Blockbench and Snowstorm plugins

Via protocol, not embedding (both upstreams appear to be GPL-3.0, to verify). Plugins talk to
`beacon serve`: open/save models and particles in the project, hot reload, id autocomplete.

## B10. Text generation for non-JSON, non-TS files (needed)

**Status: slices 1 and 2 built (ARCHITECTURE 14.6): `.mcfunction` and `.lang` expander, `if`/comparisons/methods/f-strings, symbols, migrate. Open: migrating `.lang` templates that feed a merged output (per-source attribution), dict `foreach`, `_map.py` computed scopes.** Needed, not speculative. Requested by the team on 2026-10-19 after the phase-3
migrations. A way to *generate* `.mcfunction`, `.lang` and other text files from compact
source, the way the subfunctions / system_template text templating did: loops
(`for <i 1..11 1>:`), inline expressions (`eval:`), nested function definitions, function
trees, schedules.

Why: today `beacon migrate` freezes the oracle's expansion of subfunctions syntax into static
files (14.3), so a loop of 11 becomes 11 hand-maintained copies. Generators already cover
JSON and TS; text files have no equivalent besides writing a JS generator that emits strings.

Design notes:
- Runs in the JS host like generators (same sandbox, trust and cache rules), or as a small
  built-in expander with a fixed grammar (no code), whichever keeps `check`/LSP able to see
  definitions and references through it (rename of an id inside a loop must still work).
- `beacon migrate` would then translate subfunctions syntax into this form instead of freezing
  the expansion.
- Scope: `.mcfunction` and `.lang` first; other text formats on demand.

## B9. Other

- TUI for the CLI.
- `additionalPacks` RFC to the project-config-standard (purely additive; only after
  multi-pack has users).
- Shared content modules across projects (git/npm-distributed Beacon modules).
- UUID rotation command.
- In-game photo editing (better as a separate tool).
