import { useEffect, useState } from "react";
import type { ProjectSummary } from "./api";
import { About } from "./screens/About";
import { Home } from "./screens/Home";
import { Play } from "./screens/Play";
import { Placeholder } from "./screens/Placeholder";
import { SETUP_DONE, Setup } from "./screens/Setup";

const ROUTES = ["home", "play", "settings", "setup", "about"] as const;
type Route = (typeof ROUTES)[number];

// first launch (no explicit route, wizard never finished) opens the wizard
const routeOf = (hash: string): Route =>
  (ROUTES as readonly string[]).includes(hash.slice(1))
    ? (hash.slice(1) as Route)
    : localStorage.getItem(SETUP_DONE)
      ? "home"
      : "setup";

export function App() {
  const [route, setRoute] = useState<Route>(routeOf(location.hash));
  const [project, setProject] = useState<ProjectSummary | null>(null);
  useEffect(() => {
    const on = () => setRoute(routeOf(location.hash));
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);

  return (
    <div className="shell">
      <nav aria-label="Main">
        <h1>Beacon</h1>
        {ROUTES.map((r) => (
          <a key={r} href={`#${r}`} aria-current={r === route ? "page" : undefined}>
            {r[0].toUpperCase() + r.slice(1)}
          </a>
        ))}
        {project && <p className="current">{project.name}</p>}
      </nav>
      <main>
        {route === "home" && <Home project={project} onProject={setProject} />}
        {route === "play" && <Play project={project} onProject={setProject} />}
        {route === "settings" && (
          <Placeholder title="Settings">Visual settings and module forms. Coming next.</Placeholder>
        )}
        {route === "setup" && <Setup onDone={() => (location.hash = "#home")} />}
        {route === "about" && <About />}
      </main>
    </div>
  );
}
