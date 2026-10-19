// Builds the OFFICIAL minecraft-debugger debug adapter (Mojang/minecraft-debugger, MIT) out of
// process: clones the MIT source, bundles its `Session` with `vscode` replaced by a stub, and
// serves it over a TCP socket (the same way the official extension serves it in-process).
// Nothing is downloaded from the Marketplace.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync, copyFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const work = resolve(here, "../../.e2e");
const src = join(work, "minecraft-debugger");
// The brief named c69bc37, which does not exist in upstream; c54af36 (2026-10-13) is the HEAD the
// architecture notes were verified against.
const COMMIT = process.env.MC_DEBUGGER_COMMIT ?? "c54af36";
const run = (cmd, args, cwd) => execFileSync(cmd, args, { cwd, stdio: "inherit", shell: process.platform === "win32" });

mkdirSync(work, { recursive: true });
if (!existsSync(src)) run("git", ["clone", "https://github.com/Mojang/minecraft-debugger", src], work);
run("git", ["checkout", "-q", COMMIT], src);
if (!existsSync(join(src, "node_modules"))) run("npm", ["ci", "--ignore-scripts"], src);

writeFileSync(join(work, "vscode-stub.js"), `
const noop = () => {};
const watcher = { dispose: noop, onDidChange: noop, onDidCreate: noop, onDidDelete: noop };
module.exports = {
  commands: { executeCommand: () => Promise.resolve() },
  window: { showInformationMessage: noop, showWarningMessage: noop, showErrorMessage: noop,
            showInputBox: () => Promise.resolve(undefined), showQuickPick: () => Promise.resolve(undefined) },
  workspace: { workspaceFolders: undefined, asRelativePath: (p) => p,
               getConfiguration: () => ({ get: () => undefined }), createFileSystemWatcher: () => watcher },
  Uri: { file: (p) => ({ fsPath: p }) },
  RelativePattern: class { constructor(base, pattern) { this.base = base; this.pattern = pattern; } },
};
`);
writeFileSync(join(work, "adapter-entry.ts"), `
import { createServer } from "net";
import { EventEmitter } from "events";
import { Session } from ${JSON.stringify(join(src, "src/session"))};
if (process.env.ADAPTER_TRACE) {
  const proto = Session.prototype as any;
  const orig = proto.sendDebuggeeMessage;
  const recv = proto.receiveDebugeeMessage;
  proto.receiveDebugeeMessage = function (m: unknown, ...r: unknown[]) {
    console.error("[from game]", JSON.stringify(m).slice(0, 600));
    return recv.call(this, m, ...r);
  };
  proto.sendDebuggeeMessage = function (m: unknown, ...r: unknown[]) {
    console.error("[to game]", JSON.stringify(m).slice(0, 400));
    return orig.call(this, m, ...r);
  };
}
const none: any = new Proxy({}, { get: () => () => undefined }); // HomeViewProvider / StatsProvider stubs
const server = createServer((socket) => {
  const s = new Session(none, none, new EventEmitter());
  s.setRunAsServer(true);
  s.start(socket, socket);
}).listen(Number(process.argv[2] ?? 0), "127.0.0.1", () => console.log("ADAPTER_PORT=" + (server.address() as any).port));
`);

const { build } = createRequire(join(src, "package.json"))("esbuild");
await build({
  entryPoints: [join(work, "adapter-entry.ts")],
  bundle: true, platform: "node", format: "cjs", outfile: join(work, "adapter.js"),
  alias: { vscode: join(work, "vscode-stub.js") }, logLevel: "warning",
});
copyFileSync(join(src, "node_modules/source-map/lib/mappings.wasm"), join(work, "mappings.wasm"));
console.log("adapter built:", join(work, "adapter.js"));
