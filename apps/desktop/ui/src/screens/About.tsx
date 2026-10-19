import { useEffect, useState } from "react";
import { type AppInfo, api } from "../api";

export function About() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  useEffect(() => {
    api.appInfo().then(setInfo, () => setInfo(null));
  }, []);
  return (
    <section>
      <h2>About</h2>
      {info && (
        <p>
          Beacon {info.version} ({info.platform}), engine {info.engine}
        </p>
      )}
      <p className="muted">Updates are off in this build.</p>
    </section>
  );
}
