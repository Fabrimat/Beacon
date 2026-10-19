import { test } from "node:test";
import assert from "node:assert/strict";
import { cliArgs, isBeaconManaged, merge, taskArgs, validName } from "../src/debugConfig";

const resolved = {
  type: "minecraft-js",
  request: "attach",
  name: "Beacon: dev/main",
  mode: "connect",
  host: "127.0.0.1",
  port: 19144,
  localRoot: "${workspaceFolder}/.beacon/debug/dev/main",
  sourceMapRoot: "${workspaceFolder}/.beacon/debug/dev/main",
  generatedSourceRoot: "${workspaceFolder}/.beacon/debug/dev/main",
  targetModuleUuid: "539f5412-bdc8-4c07-a2e9-27b45ad20b32",
};

test("managed entries carry beaconOutput / beaconTarget", () => {
  assert.ok(isBeaconManaged({ beaconOutput: "main" }));
  assert.ok(!isBeaconManaged({ type: "minecraft-js" }));
});

test("cliArgs passes target, output and mode", () => {
  assert.deepEqual(cliArgs({ beaconTarget: "dev", beaconOutput: "main", mode: "listen" }, "/p"), [
    "debug-config", "--json", "--project", "/p", "--target", "dev", "--output", "main", "--mode", "listen",
  ]);
  assert.deepEqual(cliArgs({ beaconOutput: "main" }, "/p").slice(4), ["--output", "main"]);
});

test("merge fills the Beacon fields and drops the beacon* keys", () => {
  const m = merge({ type: "minecraft-js", request: "attach", name: "x", beaconOutput: "main", beaconTarget: "dev" }, resolved);
  assert.equal(m.targetModuleUuid, resolved.targetModuleUuid);
  assert.equal(m.localRoot, resolved.localRoot);
  assert.equal(m.port, 19144);
  assert.equal(m.name, "x"); // user wins
  assert.ok(!("beaconOutput" in m) && !("beaconTarget" in m));
});

test("merge overrides the official provider's defaults and its port prompt", () => {
  const m = merge({ beaconOutput: "main", localRoot: "${workspaceFolder}/scripts/", inputPort: "${command:PromptForPort}" }, resolved);
  assert.equal(m.localRoot, resolved.localRoot);
  assert.ok(!("inputPort" in m));
});

test("merge keeps a user port / host", () => {
  const m = merge({ beaconOutput: "main", port: 20000, host: "10.0.0.2" }, resolved);
  assert.equal(m.port, 20000);
  assert.equal(m.host, "10.0.0.2");
});

test("taskArgs builds argv, validates the target, adds --trust only when asked", () => {
  assert.deepEqual(taskArgs("build", "/p", false), ["build", "--project", "/p"]);
  assert.deepEqual(taskArgs("watch", "/p", true, "dev"), ["watch", "--target", "dev", "--project", "/p", "--trust"]);
  for (const bad of ["--trust", "a b", "dev; calc", "../x", "", 5]) {
    assert.throws(() => taskArgs("build", "/p", false, bad), /invalid target/);
    assert.throws(() => validName(bad), /invalid/);
  }
});

test("cliArgs refuses flag-like target / output from launch.json", () => {
  assert.throws(() => cliArgs({ beaconTarget: "--trust" }, "/p"), /invalid beaconTarget/);
  assert.throws(() => cliArgs({ beaconOutput: "a/b" }, "/p"), /invalid beaconOutput/);
});
