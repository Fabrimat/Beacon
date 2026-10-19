// Pure logic of the `minecraft-js` configuration provider (no `vscode` import: unit-testable).

const BEACON_FIELDS = ["targetModuleUuid", "localRoot", "sourceMapRoot", "generatedSourceRoot"];

export type Config = Record<string, unknown>;

/** True for launch.json entries Beacon should resolve (`beaconOutput` / `beaconTarget`). */
export const isBeaconManaged = (c: Config): boolean => "beaconOutput" in c || "beaconTarget" in c;

const NAME = /^[A-Za-z0-9][A-Za-z0-9_-]*$/;

/** A target / output name as it reaches the CLI; anything else (flags, paths) is refused. */
export function validName(n: unknown, what = "name"): string {
  if (typeof n !== "string" || !NAME.test(n)) throw new Error(`invalid ${what} ${JSON.stringify(n)}: want [A-Za-z0-9][A-Za-z0-9_-]*`);
  return n;
}

/** `beacon build` / `watch` arguments (spawned without a shell). `trust` is the user's choice. */
export function taskArgs(cmd: "build" | "watch", project: string, trust: boolean, target?: unknown): string[] {
  const a: string[] = [cmd];
  if (target !== undefined) a.push("--target", validName(target, "target"));
  a.push("--project", project);
  if (trust) a.push("--trust");
  return a;
}

/** `beacon debug-config` arguments for a launch entry. */
export function cliArgs(c: Config, project: string): string[] {
  const a = ["debug-config", "--json", "--project", project];
  if (c.beaconTarget !== undefined) a.push("--target", validName(c.beaconTarget, "beaconTarget"));
  if (c.beaconOutput !== undefined) a.push("--output", validName(c.beaconOutput, "beaconOutput"));
  if (c.mode === "connect" || c.mode === "listen") a.push("--mode", c.mode);
  return a;
}

/**
 * Merges the resolved config into the launch entry. Beacon owns the roots and the module UUID
 * (the official provider may already have filled `localRoot` / `inputPort` defaults, so those
 * cannot be told from user choices). `mode`, `host`, `port` and `passcode` set by the user win.
 */
export function merge(user: Config, resolved: Config): Config {
  const out: Config = { ...resolved, ...user };
  for (const k of BEACON_FIELDS) out[k] = resolved[k] ?? out[k];
  if (out.port !== undefined) delete out.inputPort; // official default prompts for a port when none is set
  delete out.beaconOutput;
  delete out.beaconTarget;
  return out;
}
