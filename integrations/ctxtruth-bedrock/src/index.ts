import { execFile } from "node:child_process";

// ctxtruth plugin (apiVersion 1) for Beacon / Bedrock projects, see docs/ARCHITECTURE.md 8.4b.
// The ctxtruth types are declared structurally here (no dependency on ctxtruth itself).

export interface Options {
  /** The beacon executable, or `[executable, ...leadingArgs]`. Default: `beacon` from PATH. */
  beacon?: string | string[];
  /** Per Beacon CLI call. Default 10 000 ms. */
  timeoutMs?: number;
  /** Working directory (the analyzed project) for the Beacon calls. */
  cwd?: string;
}
export interface Claim { value: string }
export interface Context { readFile(path: string): Promise<string | undefined> }
export type Verdict =
  | { status: "ok" }
  | { status: "mismatch"; message: string; suggestion?: string }
  | { status: "unknown"; reason: string };
export interface Check {
  id: string;
  /** Shell fences + inline code starting with these commands (ctxtruth `commands`). */
  commands?: string[];
  /** True: depends on the machine/checkout, so unknown on past commits. */
  environment: boolean;
  matches(value: string): boolean;
  verify(claims: Claim[], ctx: Context): Promise<Verdict[]>;
}
export interface Plugin {
  name: "bedrock";
  apiVersion: 1;
  ignoreRegions: { start: string; end: string }[];
  checks: Check[];
}

type Json = any; // parsed Beacon output, validated field by field below
type Res = { ok: true; json: Json } | { ok: false; reason: string };
const unknown = (reason: string): Verdict => ({ status: "unknown", reason });
const OK: Verdict = { status: "ok" };

/** One Beacon CLI call: argv array, no shell, time-bounded. Never throws. */
function call(o: Options, args: string[]): Promise<Res> {
  const [cmd, ...lead] = ([] as string[]).concat(o.beacon ?? "beacon");
  const label = `beacon ${args.join(" ")}`;
  return new Promise((resolve) => {
    try {
      execFile(
        cmd,
        [...lead, ...args],
        { cwd: o.cwd, timeout: o.timeoutMs ?? 10_000, maxBuffer: 64 << 20, windowsHide: true },
        (err, stdout) => {
          if (err) {
            const e = err as NodeJS.ErrnoException & { killed?: boolean };
            return resolve({ ok: false, reason: e.killed ? `${label} timed out` : `${label} failed: ${e.code ?? e.message}` });
          }
          try {
            resolve({ ok: true, json: JSON.parse(stdout) });
          } catch {
            resolve({ ok: false, reason: `${label} printed malformed JSON` });
          }
        },
      );
    } catch (e) {
      resolve({ ok: false, reason: `${label} could not start: ${(e as Error).message}` });
    }
  });
}

const MAX_LOOKUPS = 50;

/** Each input is fetched at most once per `verify` (no module-level cache), lazily. */
function session(o: Options) {
  const memo = new Map<string, Promise<Res>>();
  let lookups = 0;
  const get = (key: string, args: string[], valid: (j: Json) => boolean): Promise<Res> => {
    let p = memo.get(key);
    if (!p) {
      p = call(o, args).then((r) => (r.ok && !valid(r.json) ? { ok: false, reason: `beacon ${args.join(" ")}: unexpected output shape` } : r));
      memo.set(key, p);
    }
    return p;
  };
  return {
    help: () => get("help", ["--help", "--json"], (j) => j?.schema === 1 && Array.isArray(j?.command?.commands)),
    symbols: () => get("symbols", ["symbols", "--json"], (j) => j?.schema === 1 && Array.isArray(j?.symbols)),
    // `beacon api --json` (schema 1): { gameVersion, installed, modules: { "@minecraft/server": [versions] }, vanilla: { kind: count } }.
    // Not installed (`installed:false`, or an old beacon without `api`) is unknown for API facts.
    api: async (): Promise<Res> => {
      const r = await get("api", ["api", "--json"], (j) => j?.schema === 1 && typeof j?.installed === "boolean" && typeof j?.modules === "object" && j.modules !== null);
      return r.ok && !r.json.installed ? { ok: false, reason: "no API index installed (beacon api install)" } : r;
    },
    // `beacon api lookup <q> --json`: { gameVersion, hits: [{ path, ... }] }; exact matches come first. One call per distinct
    // query per verify, at most MAX_LOOKUPS; beyond that the claim is unknown.
    lookup: (q: string): Promise<Res> =>
      !memo.has("l:" + q) && ++lookups > MAX_LOOKUPS
        ? Promise.resolve({ ok: false, reason: `more than ${MAX_LOOKUPS} API lookups in one verify` })
        : get("l:" + q, ["api", "lookup", q, "--json"], (j) => j?.schema === 1 && Array.isArray(j?.hits)),
  };
}

// ---- bedrock/commands --------------------------------------------------------------------

interface Cmd { name?: string; args: { long: string | null; short: string | null; takesValue: boolean }[]; commands: Cmd[] }

function dist(a: string, b: string): number {
  const d = Array.from({ length: a.length + 1 }, (_, i) => [i, ...Array<number>(b.length).fill(0)]);
  for (let j = 1; j <= b.length; j++) d[0][j] = j;
  for (let i = 1; i <= a.length; i++)
    for (let j = 1; j <= b.length; j++) d[i][j] = Math.min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
  return d[a.length][b.length];
}
const nearest = (w: string, cands: string[]) => cands.filter((c) => dist(w, c) <= 2).sort((x, y) => dist(w, x) - dist(w, y))[0];

function checkCommand(root: Cmd, line: string): Verdict {
  const toks = line.trim().split(/\s+/);
  const end = toks.findIndex((t) => /^(\||\|\||&&|;|>|>>|2>.*)$/.test(t));
  const words = end < 0 ? toks : toks.slice(0, end);
  const tail = end < 0 ? [] : toks.slice(end);
  if (words[0] !== "beacon") return unknown("not a beacon command");
  const chain: Cmd[] = [root];
  // ponytail: every ancestor's flags are accepted, not only the `global` ones
  const flags = () => chain.flatMap((c) => c.args);
  for (let i = 1; i < words.length; i++) {
    const w = words[i];
    if (w === "--") break;
    if (w.length > 1 && w.startsWith("-")) {
      if (w === "--help" || w === "-h") continue;
      const long = w.startsWith("--");
      const name = long ? w.slice(2).split("=")[0] : w[1];
      const eq = long && w.includes("=");
      const arg = flags().find((a) => (long ? a.long === name : a.short === name));
      if (!arg) {
        const fix = long ? nearest(name, flags().map((a) => a.long).filter((l): l is string => !!l)) : undefined;
        const where = chain.length > 1 ? ` for \`${chain.slice(1).map((c) => c.name).join(" ")}\`` : "";
        const m: Verdict = { status: "mismatch", message: `beacon has no flag \`${long ? "--" : "-"}${name}\`${where}` };
        if (fix) m.suggestion = [...words.slice(0, i), `--${fix}${eq ? w.slice(w.indexOf("=")) : ""}`, ...words.slice(i + 1), ...tail].join(" ");
        return m;
      }
      if (arg.takesValue && !eq && (long || w.length === 2)) i++; // the next word is its value
      continue;
    }
    if (/^[<\[]/.test(w)) continue; // placeholder
    const cur = chain[chain.length - 1];
    const sub = cur.commands.find((c) => c.name === w);
    if (sub) { chain.push(sub); continue; }
    if (cur.commands.length > 0) {
      const fix = nearest(w, cur.commands.map((c) => c.name!));
      const m: Verdict = { status: "mismatch", message: `beacon has no ${chain.length > 1 ? "subcommand" : "command"} \`${w}\`` };
      if (fix) m.suggestion = [...words.slice(0, i), fix, ...words.slice(i + 1), ...tail].join(" ");
      return m;
    }
    // a positional of a leaf command: not checked
  }
  return OK;
}

const commandsCheck = (o: Options): Check => ({
  id: "bedrock/commands",
  commands: ["beacon"],
  environment: true,
  matches: (v) => /^beacon(\s|$)/.test(v.trim()),
  async verify(claims) {
    const h = await session(o).help();
    if (!h.ok) return claims.map(() => unknown(h.reason));
    return claims.map((c) => checkCommand(h.json.command, c.value));
  },
});

// ---- bedrock/config ----------------------------------------------------------------------

const FACT = /^\s*(namespace|targetVersion|editions?|outputs?(?: id)?)\s*(?:[:=]|is)?\s*`?([A-Za-z0-9_.:\-]+)`?\s*$/i;

function parseConfig(text: string): Json | undefined {
  try { return JSON.parse(text); } catch { /* config.json may be JSONC */ }
  try {
    // ponytail: strips // and /* */ outside strings only; no trailing-comma repair
    return JSON.parse(text.replace(/"(?:[^"\\]|\\.)*"|\/\/[^\n]*|\/\*[\s\S]*?\*\//g, (m) => (m[0] === '"' ? m : "")));
  } catch { return undefined; }
}

const configCheck = (): Check => ({
  id: "bedrock/config",
  environment: false,
  matches: (v) => FACT.test(v),
  async verify(claims, ctx) {
    let text: string | undefined;
    try { text = await ctx.readFile("config.json"); } catch { /* unknown below */ }
    const cfg = text === undefined ? undefined : parseConfig(text);
    return claims.map((c): Verdict => {
      if (!cfg || typeof cfg !== "object") return unknown("config.json missing or unreadable");
      const m = FACT.exec(c.value);
      if (!m) return unknown("not a config fact");
      const key = m[1].toLowerCase();
      const claimed = m[2];
      let actual: string[] | undefined;
      if (key === "namespace") actual = typeof cfg.namespace === "string" ? [cfg.namespace] : undefined;
      else if (key === "targetversion") actual = typeof cfg.targetVersion === "string" ? [cfg.targetVersion] : undefined;
      else if (key.startsWith("edition")) {
        const t = cfg.beacon?.targets;
        actual = t && typeof t === "object" ? [...new Set(Object.values<Json>(t).map((x) => x?.edition).filter((e) => typeof e === "string"))] : undefined;
      } else {
        const outs = cfg.beacon?.outputs;
        actual = Array.isArray(outs) ? outs.map((x: Json) => x?.id).filter((i: unknown) => typeof i === "string") : undefined;
      }
      if (!actual || actual.length === 0) return unknown(`config.json does not set ${m[1]}`);
      if (actual.some((a) => a.toLowerCase() === claimed.toLowerCase())) return OK;
      return { status: "mismatch", message: `config.json: ${m[1]} \`${claimed}\` not found (have ${actual.join(", ")})`, ...(actual.length === 1 ? { suggestion: actual[0] } : {}) };
    });
  },
});

// ---- bedrock/ids -------------------------------------------------------------------------

const NS_ID = /^[a-z0-9_.\-]+:[a-z0-9_.\-/]+$/;
const MODULE = /^(@minecraft\/[a-z0-9-]+)(?:[#.]([A-Za-z_]\w*)(?:[#.]([A-Za-z_]\w*))?)?$/;

const idsCheck = (o: Options): Check => ({
  id: "bedrock/ids",
  environment: true,
  matches: (v) => NS_ID.test(v.trim()) || MODULE.test(v.trim()),
  async verify(claims) {
    const s = session(o);
    return Promise.all(
      claims.map(async (c): Promise<Verdict> => {
        const v = c.value.trim();
        const mod = MODULE.exec(v);
        if (mod) return moduleVerdict(s, mod);
        if (v.startsWith("minecraft:")) {
          const api = await s.api();
          if (!api.ok) return unknown(api.reason);
          const l = await s.lookup(v);
          if (!l.ok) return unknown(l.reason);
          // an exact id: `vanilla:<kind>#<id>` or `component:<kind>#<id>`
          const hit = l.json.hits.some((h: Json) => typeof h?.path === "string" && /^(vanilla|component):[^#]+#/.test(h.path) && h.path.split("#")[1] === v);
          return hit ? OK : { status: "mismatch", message: `\`${v}\` is not a vanilla id or component in API index ${api.json.gameVersion ?? "?"}` };
        }
        const sym = await s.symbols();
        if (!sym.ok) return unknown(sym.reason);
        const syms: Json[] = sym.json.symbols;
        if (syms.some((x) => x?.name === v)) return OK;
        // Only the project's own namespaces are authoritative; a foreign namespace may come from another pack.
        const ns = v.split(":")[0];
        const own = syms.some((x) => ["entity", "item", "block"].includes(x?.kind) && typeof x?.name === "string" && x.name.startsWith(ns + ":"));
        return own ? { status: "mismatch", message: `\`${v}\` is not defined in this project (namespace \`${ns}\` is)` } : unknown(`namespace \`${ns}\` is not defined in this project`);
      }),
    );
  },
});

async function moduleVerdict(s: ReturnType<typeof session>, m: RegExpExecArray): Promise<Verdict> {
  const api = await s.api();
  if (!api.ok) return unknown(api.reason);
  const [, name, exp, member] = m;
  if (!Array.isArray(api.json.modules[name])) return unknown(`\`${name}\` is not in the API index`);
  if (!exp) return OK;
  const key = member ? `${exp}.${member}` : exp;
  const l = await s.lookup(key);
  if (!l.ok) return unknown(l.reason);
  const v = api.json.gameVersion ?? "?";
  // the lookup is module-agnostic: the hit must be in the claimed module (any installed version)
  const hit = l.json.hits.some((h: Json) => typeof h?.path === "string" && h.path.startsWith(name + "@") && h.path.split("#")[1] === key);
  return hit ? OK : { status: "mismatch", message: `\`${name}\` has no \`${key}\` in API index ${v}` };
}

export default function bedrock(options: Options = {}): Plugin {
  return {
    name: "bedrock",
    apiVersion: 1,
    ignoreRegions: [{ start: "<!-- beacon:generated:start -->", end: "<!-- beacon:generated:end -->" }],
    checks: [commandsCheck(options), configCheck(), idsCheck(options)],
  };
}
// require.resolve consumers (ctxtruth) get the factory directly, import consumers keep `.default`.
module.exports = Object.assign(bedrock, { default: bedrock });
