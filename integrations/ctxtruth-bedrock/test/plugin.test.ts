import { test } from "node:test";
import assert from "node:assert/strict";
import { join } from "node:path";
import bedrock, { Options, Verdict } from "../src/index";

const fake = join(__dirname, "../../test/fake-beacon.mjs");
const opts = (timeoutMs?: number): Options => ({ beacon: [process.execPath, fake], timeoutMs });
const CONFIG = JSON.stringify({
  namespace: "demo",
  targetVersion: "1.26.50",
  beacon: { targets: { dev: { edition: "bedrock" } }, outputs: [{ id: "main" }] },
});
const ctx = (config: string | null = CONFIG) => ({ readFile: async () => config ?? undefined });

async function run(id: string, values: string[], o = opts(), env = "", c = ctx()): Promise<Verdict[]> {
  process.env.FAKE = env;
  const check = bedrock(o).checks.find((x) => x.id === id)!;
  return check.verify(values.map((value) => ({ value })), c);
}
const st = (v: Verdict[]) => v.map((x) => x.status);

test("plugin shape", () => {
  const p = bedrock();
  assert.equal(p.name, "bedrock");
  assert.equal(p.apiVersion, 1);
  assert.deepEqual(p.checks.map((c) => c.id), ["bedrock/commands", "bedrock/config", "bedrock/ids"]);
  assert.deepEqual(p.checks.map((c) => c.environment), [true, false, true]);
  assert.equal(p.ignoreRegions[0].start, "<!-- beacon:generated:start -->");
});

test("commands: existing subcommands and flags pass", async () => {
  const v = await run("bedrock/commands", ["beacon build --target dev --trust", "beacon --project x plugin update --yes", "beacon check | tee out"]);
  assert.deepEqual(st(v), ["ok", "ok", "ok"]);
});

test("commands: unknown flag and subcommand mismatch with a full corrected line", async () => {
  const [flag, sub] = await run("bedrock/commands", ["beacon build --targt dev", "beacon bild --all"]);
  assert.equal(flag.status, "mismatch");
  assert.equal((flag as { suggestion: string }).suggestion, "beacon build --target dev");
  assert.equal(sub.status, "mismatch");
  assert.equal((sub as { suggestion: string }).suggestion, "beacon build --all");
});

test("config: facts match config.json, mismatch suggests the value, absent is unknown", async () => {
  const v = await run("bedrock/config", ["namespace: demo", "namespace: other", "targetVersion 1.26.50", "editions: bedrock", "edition: education", "output main", "outputs: nope"]);
  assert.deepEqual(st(v), ["ok", "mismatch", "ok", "ok", "mismatch", "ok", "mismatch"]);
  assert.equal((v[1] as { suggestion: string }).suggestion, "demo");
  assert.deepEqual(st(await run("bedrock/config", ["namespace: x"], opts(), "", ctx(null))), ["unknown"]);
  assert.deepEqual(st(await run("bedrock/config", ["namespace: x"], opts(), "", ctx("{broken"))), ["unknown"]);
  assert.deepEqual(st(await run("bedrock/config", ["namespace: x"], opts(), "", ctx(JSON.stringify({})))), ["unknown"]);
});

test("ids: project ids come from symbols", async () => {
  const v = await run("bedrock/ids", ["demo:car", "demo:ghost", "other:thing"]);
  assert.deepEqual(st(v), ["ok", "mismatch", "unknown"]);
});

test("ids: vanilla ids, components and @minecraft members come from api lookup", async () => {
  const v = await run("bedrock/ids", ["minecraft:zombie", "minecraft:health", "minecraft:zombie_head", "minecraft:nope", "@minecraft/server", "@minecraft/server.Player", "@minecraft/server.Player.name", "@minecraft/server#Player.makeItUp", "@minecraft/server.Wrld", "@minecraft/server.World", "@minecraft/other"]);
  // zombie_head is only a prefix hit of another query; `World` exists but in another module
  assert.deepEqual(st(v), ["ok", "ok", "mismatch", "mismatch", "ok", "ok", "ok", "mismatch", "mismatch", "mismatch", "unknown"]);
});

test("missing api index (old beacon, not installed) is unknown for api facts, symbols still work", async () => {
  for (const env of ["noapi", "uninstalled"]) {
    const v = await run("bedrock/ids", ["minecraft:zombie", "@minecraft/server.Player", "demo:car"], opts(), env);
    assert.deepEqual(st(v), ["unknown", "unknown", "ok"], env);
  }
});

test("lookups are capped per verify: beyond the cap the claim is unknown", async () => {
  const ids = Array.from({ length: 52 }, (_, i) => `minecraft:x${i}`);
  const v = await run("bedrock/ids", ids);
  assert.equal(v.filter((x) => x.status === "mismatch").length, 50);
  assert.equal(v.filter((x) => x.status === "unknown").length, 2);
});

test("timeout is unknown, not a pass, and does not crash", async () => {
  const t = Date.now();
  const v = await run("bedrock/commands", ["beacon build"], opts(300), "hang");
  assert.equal(v[0].status, "unknown");
  assert.match((v[0] as { reason: string }).reason, /timed out/);
  assert.ok(Date.now() - t < 5000);
  assert.deepEqual(st(await run("bedrock/ids", ["demo:car", "minecraft:zombie"], opts(300), "hang")), ["unknown", "unknown"]);
});

test("malformed JSON is unknown", async () => {
  assert.deepEqual(st(await run("bedrock/commands", ["beacon build"], opts(), "garbage")), ["unknown"]);
  assert.deepEqual(st(await run("bedrock/ids", ["demo:car", "minecraft:zombie"], opts(), "garbage")), ["unknown", "unknown"]);
});

test("a missing beacon executable is unknown", async () => {
  const v = await run("bedrock/commands", ["beacon build"], { beacon: "beacon-does-not-exist-xyz", timeoutMs: 2000 });
  assert.equal(v[0].status, "unknown");
});
