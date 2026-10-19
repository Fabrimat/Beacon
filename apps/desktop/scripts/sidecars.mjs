// Produces src-tauri/binaries/{beacon,esbuild}-<target-triple>[.exe]: the two externalBins.
//  1. beacon: `cargo build --release -p beacon-cli` in the repo root workspace.
//  2. esbuild: the pinned, blake3-verified download, done by that beacon itself
//     (`beacon setup esbuild --yes` into a scratch BEACON_HOME), never an unverified fetch.
// Env: CARGO_TARGET_DIR (optional), BEACON_ESBUILD_BIN (optional: use this esbuild binary
// instead of downloading; local use only).
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const out = resolve(here, "../src-tauri/binaries");
const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { stdio: "inherit", ...opts });

const triple = /host: (\S+)/.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))[1];
const exe = triple.includes("windows") ? ".exe" : "";
mkdirSync(out, { recursive: true });

run("cargo", ["build", "--locked", "--release", "-p", "beacon-cli"], { cwd: repo });
const target = process.env.CARGO_TARGET_DIR ?? join(repo, "target");
copyFileSync(join(target, "release", `beacon${exe}`), join(out, `beacon-${triple}${exe}`));

let esbuild = process.env.BEACON_ESBUILD_BIN;
if (!esbuild) {
  const home = mkdtempSync(join(tmpdir(), "beacon-sidecar-"));
  run(join(target, "release", `beacon${exe}`), ["setup", "esbuild", "--yes"], {
    env: { ...process.env, BEACON_HOME: home, BEACON_DOWNLOAD: "yes" },
  });
  const tools = join(home, "tools");
  const name = readdirSync(tools).find((f) => /^esbuild-[\d.]+-.+?(\.exe)?$/.test(f) && !f.endsWith(".blake3"));
  if (!name) throw new Error(`no esbuild-* in ${tools}`);
  esbuild = join(tools, name);
}
copyFileSync(esbuild, join(out, `esbuild-${triple}${exe}`));
console.log(`sidecars for ${triple} in ${out}`);
