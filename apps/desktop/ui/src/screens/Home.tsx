import { useState } from "react";
import { type ProjectSummary, api, errorText } from "../api";
import { NewProject } from "./NewProject";
import { TrustPrompt } from "./TrustPrompt";

const RECENT = "beacon.recent";
const recent = (): string[] => {
  try {
    return JSON.parse(localStorage.getItem(RECENT) ?? "[]");
  } catch {
    return [];
  }
};
const remember = (root: string) =>
  localStorage.setItem(RECENT, JSON.stringify([root, ...recent().filter((r) => r !== root)].slice(0, 5)));

// ponytail: the project folder is typed (or picked from recents). A native folder dialog needs
// the dialog plugin and its permission, which would widen the capability set beyond COMMANDS.
export function Home({
  project,
  onProject,
}: {
  project: ProjectSummary | null;
  onProject: (p: ProjectSummary | null) => void;
}) {
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);

  const open = async (root: string) => {
    setError(null);
    try {
      const p = await api.projectOpen(root);
      remember(p.root);
      onProject(p);
    } catch (e) {
      setError(errorText(e));
    }
  };

  return (
    <section>
      <nav className="card" aria-label="Quick links">
        <a href="#play">Play</a> · <a href="#settings">Settings</a> · <a href="#about">About</a>
      </nav>
      <h2>Projects</h2>
      {project ? (
        <div className="card">
          <h3>{project.name}</h3>
          <p className="muted">{project.root}</p>
          <p>
            Targets: {project.targets.length ? project.targets.join(", ") : "none"}
            {project.playTarget && <> (Play builds {project.playTarget})</>}
          </p>
          {project.trust === "untrusted" ? (
            <TrustPrompt
              root={project.root}
              onDone={(trust) => onProject({ ...project, trust })}
              onCancel={() => onProject(null)}
            />
          ) : (
            <p>{project.trust === "trusted" ? "Trusted." : "No code to run: nothing to trust."}</p>
          )}
          <button className="secondary" onClick={() => onProject(null)}>
            Close project
          </button>
        </div>
      ) : (
        <>
          <form
            className="card"
            onSubmit={(e) => {
              e.preventDefault();
              open(path);
            }}
            aria-label="Open project"
          >
            <h3>Open a project</h3>
            <label>
              Project folder
              <input value={path} onChange={(e) => setPath(e.target.value)} placeholder="C:\Users\you\my-addon" required />
            </label>
            {error && <p role="alert">{error}</p>}
            <button type="submit">Open</button>
            {recent().length > 0 && (
              <ul className="recent">
                {recent().map((r) => (
                  <li key={r}>
                    <button type="button" className="link" onClick={() => open(r)}>
                      {r}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </form>
          <NewProject
            onCreated={(p) => {
              remember(p.root);
              onProject(p);
            }}
          />
        </>
      )}
    </section>
  );
}
