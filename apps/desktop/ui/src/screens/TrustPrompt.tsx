import { useEffect, useState } from "react";
import { type TrustPrompt as Prompt, type TrustState, api, errorText } from "../api";

/** What will run, and the one decision. The key goes back to the app exactly as received. */
export function TrustPrompt({
  root,
  onDone,
  onCancel,
}: {
  root: string;
  onDone: (state: TrustState) => void;
  onCancel: () => void;
}) {
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [changed, setChanged] = useState(false);

  const load = () => api.trustPrompt(root).then(setPrompt, (e) => setError(errorText(e)));
  useEffect(() => {
    load();
  }, [root]);

  const grant = async () => {
    if (!prompt) return;
    try {
      const state = await api.trustGrant(root, prompt.key);
      if (state === "trusted") onDone(state);
      else {
        // the project changed since the prompt: show what it is now
        setChanged(true);
        await load();
      }
    } catch (e) {
      setError(errorText(e));
    }
  };

  return (
    <section className="card" aria-label="Trust this project?">
      <h3>Trust this project?</h3>
      {error && <p role="alert">{error}</p>}
      {changed && <p role="alert">The project changed while this was open. Please review it again.</p>}
      {prompt && (
        <>
          <p>Trusting it lets Beacon run the following on this computer:</p>
          <ul>
            {prompt.runs.map((r, i) => (
              <li key={i}>{r}</li>
            ))}
          </ul>
          {prompt.plugins.length > 0 && (
            <details>
              <summary>Details</summary>
              <pre>{JSON.stringify(prompt.plugins, null, 2)}</pre>
            </details>
          )}
          <div className="row">
            <button onClick={grant}>Trust and continue</button>
            <button className="secondary" onClick={onCancel}>
              Not now
            </button>
          </div>
        </>
      )}
    </section>
  );
}
