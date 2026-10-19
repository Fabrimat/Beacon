import { useEffect, useState } from "react";
import { type AppInfo, api } from "../api";

// The build channel is the pre-release tag of the app version (`0.1.0-alpha.1` -> alpha).
const channel = (version: string) => version.split("-")[1]?.split(".")[0] ?? "stable";

export function About() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  useEffect(() => {
    api.appInfo().then(setInfo, () => setInfo(null));
  }, []);
  return (
    <section>
      <h2>About</h2>
      {info && (
        <ul>
          <li>
            Beacon {info.version} ({info.platform})
          </li>
          <li>beacon CLI {info.engine}</li>
          <li>Channel: {channel(info.version)}</li>
        </ul>
      )}
      <h3>Updates are off in this build</h3>
      <p>This is an unsigned alpha with no update feed, so Beacon never checks for or installs updates.</p>
      <p className="muted">To get a newer build, ask the person who gave you this installer.</p>
    </section>
  );
}
