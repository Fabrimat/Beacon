# Beacon plugin for Claude Code

Enforces the guardrails of ARCHITECTURE 8.2 in Claude Code sessions inside a Beacon project:

- **PreToolUse** (`Edit|Write|MultiEdit|NotebookEdit`): denies edits to `beacon.lock.json`,
  generated `manifest.json` files (`packs/*`, `.beacon/out`, dev pack folders) and the
  `AGENTS.md` text between the `beacon:generated` markers, and points to the owning command.
  Everything else is left to your normal permission settings.
- **Stop**: runs `beacon check` in the project (restricted mode unless the project is trusted)
  and, if it reports errors, asks the agent to fix them. It does nothing when Claude is already
  continuing because of a stop hook, and nothing outside a Beacon project.

Both hooks fail open: unreadable input or a missing `beacon` never blocks a session.

## Install

1. Put `beacon` on your `PATH` (`beacon --version` must work in the shell Claude Code uses).
2. Add this folder as a plugin, from a Beacon checkout:

   ```
   claude --plugin-dir /path/to/beacon/integrations/claude-code
   ```

   or publish it through a plugin marketplace and `/plugin install beacon`.
3. Check `/hooks` lists the two Beacon hooks.

Schema reference: <https://code.claude.com/docs/en/hooks>,
<https://code.claude.com/docs/en/plugins-reference>.
