import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { type ProjectSummary, errorText } from "../api";
import { TrustPrompt } from "./TrustPrompt";

interface Diag {
  code: string;
  severity: "error" | "warning" | "note";
  message: string;
}
interface PlayReport {
  diagnostics: Diag[];
  deployedTo: string | null;
  launched: boolean;
}
interface LogLine {
  level: string;
  text: string;
}
interface Progress {
  task: string;
  step: string;
  percent: number | null;
}

const MAX_LOG = 500;
const NEEDS_TRUST = "BCN0050";
const NO_MINECRAFT = "BCN0200";

/** The one thing to tell the user: a build error, or "no Minecraft" (a warning, but the reason Play cannot work). */
const headline = (d: Diag[]) =>
  d.find((x) => x.code === NEEDS_TRUST || x.code === NO_MINECRAFT || x.severity === "error");

/** Play: build + deploy the dev target through the daemon, launch the game, show its content log.
 *  All text (diagnostics, log lines) arrives sanitized and is rendered as React text only. */
export function Play({
  project,
  onProject,
}: {
  project: ProjectSummary | null;
  onProject: (p: ProjectSummary | null) => void;
}) {
  const [target, setTarget] = useState<string | undefined>(project?.playTarget ?? undefined);
  const [busy, setBusy] = useState(false);
  const [step, setStep] = useState<string | null>(null);
  const [report, setReport] = useState<PlayReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [trusting, setTrusting] = useState(false);
  const [log, setLog] = useState<LogLine[]>([]);

  // the panel only listens while this screen is open; the app side stops its tail on a timeout
  useEffect(() => {
    const subs = [
      listen<LogLine>("beacon://content-log", (e) =>
        setLog((l) => [...l, e.payload].slice(-MAX_LOG)),
      ),
      listen<Progress>("beacon://progress", (e) => {
        if (e.payload.task === "play") setStep(e.payload.step);
      }),
    ];
    return () => subs.forEach((s) => s.then((off) => off()));
  }, []);

  if (!project) return <p className="muted">Open a project on Home first.</p>;

  const play = async () => {
    setBusy(true);
    setReport(null);
    setError(null);
    setLog([]);
    try {
      setReport(await invoke<PlayReport>("play", { root: project.root, target }));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
      setStep(null);
    }
  };

  const top = report && headline(report.diagnostics);
  const rest = report ? report.diagnostics.filter((d) => d !== top) : [];
  return (
    <section>
      <h2>Play</h2>
      {project.targets.length > 1 && (
        <label>
          Target
          <select value={target} onChange={(e) => setTarget(e.target.value)}>
            {project.targets.map((t) => (
              <option key={t}>{t}</option>
            ))}
          </select>
        </label>
      )}
      <button onClick={play} disabled={busy}>
        {busy ? "Building..." : "Play"}
      </button>
      {busy && <p role="status">{step ?? "Building and deploying..."}</p>}
      {error && <p role="alert">{error}</p>}
      {top && (
        <div role="alert" className="card">
          <h3>{top.code === NEEDS_TRUST ? "Trust this project to Play" : top.message}</h3>
          <p className="muted">{top.code}</p>
          {top.code === NEEDS_TRUST && (
            <button onClick={() => setTrusting(true)}>Trust this project</button>
          )}
        </div>
      )}
      {trusting && (
        <TrustPrompt
          root={project.root}
          onDone={(trust) => {
            setTrusting(false);
            setReport(null);
            onProject({ ...project, trust });
          }}
          onCancel={() => setTrusting(false)}
        />
      )}
      {report && !top && (
        <p role="status">
          {report.launched ? "Deployed. Minecraft is starting." : "Deployed. Start Minecraft to try it."}
        </p>
      )}
      {rest.length > 0 && (
        <ul aria-label="Diagnostics">
          {rest.map((d, i) => (
            <li key={i}>
              {d.code}: {d.message}
            </li>
          ))}
        </ul>
      )}
      <h3>Content log</h3>
      <pre aria-label="Content log" className="log">
        {log.length === 0
          ? "Nothing yet. The game writes its content log when Settings > Creator > Enable Content Log File is on."
          : log.map((l, i) => (
              <div key={i} data-level={l.level}>
                {l.text}
              </div>
            ))}
      </pre>
    </section>
  );
}
