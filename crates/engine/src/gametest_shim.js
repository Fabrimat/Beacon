// Beacon `beacon test` runner shim (engine::gametest). `beacon test` bundles it in place of
// `@minecraft/server-gametest` (an esbuild alias), so it sees the pack's own registrations; the
// real module is imported under a placeholder specifier that Beacon renames back after bundling.
// It selects the tests matching the filter (tag `beacon:run`), and prints one
// `BEACON_TEST <json>` line per event: registered, start, done (every selected test completed).
// Verdicts come from the game's own `onTestPassed` / `onTestFailed` lines: the API exposes only
// `Test.isCompleted()`, not whether a test passed.
import * as gt from "beacon-real-server-gametest";
import { system } from "@minecraft/server";
export * from "beacon-real-server-gametest";

const FILTER = __BEACON_FILTER__;
const TAG = "beacon:run";
const emit = (e) => console.warn("BEACON_TEST " + JSON.stringify(e));
// glob: `*` any run, `?` one character; everything else literal (as \uXXXX, no escaping rules)
const glob = (g) =>
  new RegExp(
    "^" +
      g
        .split("")
        .map((c) =>
          c === "*" ? ".*" : c === "?" ? "." : "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0"),
        )
        .join("") +
      "$",
  );
const want = FILTER === null ? null : glob(FILTER);
let selected = 0;
let finished = 0;

function wrap(cls, name, fn) {
  const full = `${cls}:${name}`;
  const on = want === null || want.test(full);
  if (on) {
    selected++;
    emit({ event: "registered", name: full });
  }
  const run = (test) => {
    emit({ event: "start", name: full });
    const id = system.runInterval(() => {
      let done = true;
      try {
        done = test.isCompleted();
      } catch {}
      if (!done) return;
      system.clearRun(id);
      if (++finished === selected) emit({ event: "done" });
    });
    return fn(test);
  };
  return [on, run];
}

export function register(cls, name, fn) {
  const [on, run] = wrap(cls, name, fn);
  const b = gt.register(cls, name, run);
  return on ? b.tag(TAG) : b;
}

export function registerAsync(cls, name, fn) {
  const [on, run] = wrap(cls, name, fn);
  const b = gt.registerAsync(cls, name, run);
  return on ? b.tag(TAG) : b;
}
