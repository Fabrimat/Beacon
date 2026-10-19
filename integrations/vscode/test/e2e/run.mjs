// e2e spike (6.6): official minecraft-debugger adapter (built by build-adapter.mjs) <-DAP-> this
// client; adapter <-wire protocol-> BDS 1.26.52.3 with script-debugger-auto-attach=listen.
// Gates, printed as they pass: adapter-started, attached, verified, stopped.
// env: BEACON (beacon executable, default `beacon`), BDS_ZIP (cached BDS zip, see crates/bds),
//      E2E_TIMEOUT (seconds, default 150).
import { spawn, execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, readdirSync, readFileSync, writeFileSync, mkdirSync, rmSync } from "node:fs";
import { createConnection } from "node:net";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const fixture = resolve(here, "../fixture");
const adapterJs = resolve(here, "../../.e2e/adapter.js");
const beacon = process.env.BEACON ?? "beacon";
const BDS_HASH = "03d640d7268fca93be29a1f27c5094f76188e9df68a0bb70f62184c9841840d9";
const bdsZip =
  process.env.BDS_ZIP ??
  join(process.env.BEACON_HOME ?? join(process.env.LOCALAPPDATA ?? join(homedir(), ".local/share"), "beacon"), "tools", BDS_HASH);
const deadline = Date.now() + Number(process.env.E2E_TIMEOUT ?? 150) * 1000;
const gate = (g) => console.log(`GATE ${g}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const cleanup = [];

async function main() {
  if (!existsSync(adapterJs)) throw new Error("run `node test/e2e/build-adapter.mjs` first");
  if (!existsSync(bdsZip)) throw new Error(`BDS zip not cached at ${bdsZip} (cargo bds smoke --accept-eula --yes caches it)`);
  const work = mkdtempSync(join(tmpdir(), "beacon-vscode-e2e-"));
  cleanup.push(() => rmSync(work, { recursive: true, force: true, maxRetries: 5 }));

  // 1. build the fixture (dev target) and read the resolved debug config the extension would get
  const out = join(work, "out");
  execFileSync(beacon, ["build", "--project", fixture, "--target", "dev", "--out", out, "--trust", "--yes"], { stdio: "inherit" });
  const cfg = JSON.parse(execFileSync(beacon, ["debug-config", "--json", "--project", fixture, "--absolute"], { encoding: "utf8" }));
  console.log("debug-config:", JSON.stringify(cfg));

  // 2. BDS: extract, install the dev BP, enable the debugger with auto-attach=listen
  const bds = join(work, "bds");
  mkdirSync(bds);
  execFileSync(process.platform === "win32" ? join(process.env.SystemRoot, "System32", "tar.exe") : "tar", ["-xf", bdsZip, "-C", bds]);
  const bpDir = join(out, "development_behavior_packs");
  const bp = readdirSync(bpDir)[0];
  const manifest = JSON.parse(readFileSync(join(bpDir, bp, "manifest.json"), "utf8"));
  cpSync(join(bpDir, bp), join(bds, "behavior_packs", bp), { recursive: true });
  const world = join(bds, "worlds", "e2e");
  mkdirSync(world, { recursive: true });
  writeFileSync(join(world, "world_behavior_packs.json"), JSON.stringify([{ pack_id: manifest.header.uuid, version: manifest.header.version }]));
  writeFileSync(
    join(bds, "server.properties"),
    [
      "allow-inbound-script-debugging=true",
      `force-inbound-debug-port=${cfg.port}`,
      "script-debugger-auto-attach=listen",
      "script-debugger-auto-attach-timeout=120",
      "level-name=e2e",
      "max-players=1",
      "online-mode=false",
      "allow-list=false",
      "server-port=41234",
      "server-portv6=41235",
      "enable-lan-visibility=false",
      "",
    ].join("\n"),
  );
  const srv = spawn(join(bds, process.platform === "win32" ? "bedrock_server.exe" : "bedrock_server"), [], {
    cwd: bds,
    env: { ...process.env, LD_LIBRARY_PATH: "." },
  });
  cleanup.push(() => srv.kill());
  const bdsLog = [];
  for (const s of [srv.stdout, srv.stderr])
    s.on("data", (d) => {
      bdsLog.push(String(d));
      process.stdout.write(`[bds] ${d}`);
    });

  // 3. the official adapter, out of process, over a socket
  const ad = spawn(process.execPath, [adapterJs, "0"], { cwd: dirname(adapterJs) });
  cleanup.push(() => ad.kill());
  ad.stderr.on("data", (d) => process.stderr.write(`[adapter] ${d}`));
  const port = await new Promise((res, rej) => {
    ad.stdout.on("data", (d) => {
      const m = /ADAPTER_PORT=(\d+)/.exec(String(d));
      if (m) res(Number(m[1]));
    });
    ad.on("exit", () => rej(new Error("adapter exited")));
  });
  gate("adapter-started");

  // 4. DAP client (Content-Length framed JSON)
  const sock = createConnection(port, "127.0.0.1");
  sock.on("error", () => {});
  let seq = 1;
  let buf = Buffer.alloc(0);
  const pending = new Map();
  const events = [];
  const waiters = [];
  sock.on("data", (d) => {
    buf = Buffer.concat([buf, d]);
    for (;;) {
      const h = buf.indexOf("\r\n\r\n");
      if (h < 0) return;
      const len = Number(/Content-Length: (\d+)/i.exec(buf.subarray(0, h).toString())[1]);
      if (buf.length < h + 4 + len) return;
      const m = JSON.parse(buf.subarray(h + 4, h + 4 + len).toString());
      buf = buf.subarray(h + 4 + len);
      if (m.type === "response") pending.get(m.request_seq)?.(m);
      else if (m.type === "event") {
        events.push(m);
        console.log("[dap event]", m.event, JSON.stringify(m.body ?? {}).slice(0, 200));
        waiters.splice(0).forEach((w) => w());
      }
    }
  });
  const req = (command, args) =>
    new Promise((res) => {
      const s = seq++;
      pending.set(s, res);
      const j = JSON.stringify({ seq: s, type: "request", command, arguments: args });
      sock.write(`Content-Length: ${Buffer.byteLength(j)}\r\n\r\n${j}`);
    });
  const event = async (name) => {
    for (;;) {
      const e = events.find((x) => x.event === name);
      if (e) return e;
      if (Date.now() > deadline) throw new Error(`timeout waiting for DAP event ${name}`);
      await Promise.race([new Promise((r) => waiters.push(r)), sleep(1000)]);
    }
  };

  // the game listens only once the world loads and then waits (clamped to ~10 s) for the
  // debugger; a probe connection would consume the attach, so wait for the log line instead
  while (!bdsLog.some((l) => l.includes("Debugger auto-attach waiting"))) {
    if (Date.now() > deadline) throw new Error("BDS never started waiting for the debugger");
    await sleep(100);
  }
  gate("bds-listening");
  await req("initialize", { adapterID: "minecraft-js", linesStartAt1: true, columnsStartAt1: true, pathFormat: "path" });
  const att = await req("attach", {
    type: "minecraft-js",
    request: "attach",
    mode: cfg.mode,
    host: cfg.host,
    port: cfg.port,
    localRoot: cfg.localRoot,
    sourceMapRoot: cfg.sourceMapRoot,
    generatedSourceRoot: cfg.generatedSourceRoot,
    targetModuleUuid: cfg.targetModuleUuid,
  });
  if (!att.success) throw new Error(`attach failed: ${JSON.stringify(att)}`);
  gate("attached");
  await event("initialized");

  const ts = join(fixture, "src/main/scripts/main.ts");
  const line = readFileSync(ts, "utf8").split("\n").findIndex((l) => l.includes("// BREAKPOINT_LINE")) + 1;
  const sb = await req("setBreakpoints", { source: { path: ts }, breakpoints: [{ line }] });
  console.log("setBreakpoints:", JSON.stringify(sb.body));
  if (!sb.body?.breakpoints?.[0]?.verified) throw new Error("breakpoint not verified (the map did not bind the TS line)");
  gate("verified");
  await req("configurationDone", {});
  const st = await event("stopped");
  const stack = await req("stackTrace", { threadId: st.body.threadId });
  const top = stack.body?.stackFrames?.[0];
  console.log("top frame:", JSON.stringify(top));
  if (top?.line !== line) throw new Error(`stopped at ${top?.source?.path}:${top?.line}, wanted line ${line}`);
  gate("stopped");
  await req("disconnect", {});
}

main()
  .catch((e) => {
    console.error(`FAIL ${e.message}`);
    process.exitCode = 1;
  })
  .finally(() => {
    for (const c of cleanup.reverse()) {
      try {
        c();
      } catch {}
    }
    setTimeout(() => process.exit(), 500);
  });
