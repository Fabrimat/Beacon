import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { type SetupReport, type SetupStep, type Tool, api, errorText } from "../api";

export const SETUP_DONE = "beacon.setupDone";

const NAME: Record<Tool, string> = {
  esbuild: "esbuild (script bundler)",
  apiIndex: "API index (API checks)",
  bds: "Bedrock Dedicated Server (beacon test)",
};
const mb = (b: number) => `${(b / 1048576).toFixed(1)} MB`;

// the CLI's `BCN0200` text (crates/cli/src/setup.rs); the report carries no diagnostics list
const NOT_FOUND =
  "BCN0200: No Minecraft install found. Start Minecraft once, sign in and reach the main menu, then detect again.";

export function Setup({ onDone }: { onDone: () => void }) {
  const [report, setReport] = useState<SetupReport | null>(null);
  const [picked, setPicked] = useState<Partial<Record<Tool, boolean>>>({});
  const [eula, setEula] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const detect = async () => {
    setError(null);
    try {
      const r = await api.setupStatus();
      setReport(r);
      // BDS is never preselected; the rest follow the report
      setPicked(
        Object.fromEntries(
          r.steps.map((s) => [s.tool, s.tool !== "bds" && s.defaultSelected && s.status === "missing"]),
        ),
      );
    } catch (e) {
      setError(errorText(e));
    }
  };
  useEffect(() => {
    detect();
    const un = listen<{ task: string; step: string }>("beacon://progress", (e) => {
      if (e.payload.task === "setup") setBusy(e.payload.step);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const finish = () => {
    localStorage.setItem(SETUP_DONE, "1");
    onDone();
  };

  const steps = report?.steps ?? [];
  const accepted = steps.filter((s) => s.status === "missing" && picked[s.tool]).map((s) => s.tool);
  const blocked = accepted.includes("bds") && !eula;

  const cont = async () => {
    if (accepted.length === 0) return finish();
    setBusy("starting");
    setError(null);
    try {
      const r = await api.setupRun(accepted, eula);
      setReport(r);
      const bad = r.steps.filter((s) => s.status === "failed");
      if (bad.length === 0) return finish();
      setError(bad.map((s) => s.diagnostic?.message ?? `${NAME[s.tool]} failed`).join(" "));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  const row = (s: SetupStep, disabled = false) => (
    <li key={s.tool}>
      <label>
        <input
          type="checkbox"
          checked={!!picked[s.tool]}
          disabled={disabled || s.status !== "missing"}
          onChange={(e) => setPicked({ ...picked, [s.tool]: e.target.checked })}
        />{" "}
        {NAME[s.tool]} ({mb(s.bytes)}) {s.status !== "missing" && <span className="muted">{s.status}</span>}
      </label>
    </li>
  );
  const esbuild = steps.find((s) => s.tool === "esbuild");

  return (
    <section>
      <h2>Setup</h2>
      <div className="card">
        <h3>Minecraft</h3>
        {report && report.installs.length === 0 && <p role="alert">{NOT_FOUND}</p>}
        {report && report.installs.length > 0 && (
          <ul>
            {report.installs.map((i) => (
              <li key={i.comMojang}>
                {i.product} ({i.layout}): <span className="muted">{i.comMojang}</span>
              </li>
            ))}
          </ul>
        )}
        <button type="button" className="secondary" onClick={detect} disabled={!!busy}>
          Detect Minecraft
        </button>
        {esbuild &&
          (esbuild.status === "missing" ? (
            <ul>{row(esbuild)}</ul>
          ) : (
            <p className="muted">Script bundler: {esbuild.status}.</p>
          ))}
      </div>
      <details className="card">
        <summary>Optional tools</summary>
        <ul>{steps.filter((s) => s.tool !== "esbuild").map((s) => row(s, s.tool === "bds" && !eula))}</ul>
        <label>
          <input
            type="checkbox"
            checked={eula}
            onChange={(e) => {
              setEula(e.target.checked);
              if (!e.target.checked) setPicked({ ...picked, bds: false });
            }}
          />{" "}
          I accept the Minecraft EULA (needed only for the test server)
        </label>
      </details>
      {error && <p role="alert">{error}</p>}
      {busy && <p aria-live="polite">Working: {busy}...</p>}
      <button type="button" onClick={cont} disabled={!!busy || blocked}>
        Continue
      </button>
    </section>
  );
}
