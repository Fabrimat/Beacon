import { useState } from "react";
import { type ProjectSummary, api, errorText } from "../api";

/** init_create with the built-in minimal template: a trusted project in an empty folder. */
export function NewProject({ onCreated }: { onCreated: (p: ProjectSummary) => void }) {
  const [folder, setFolder] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const create = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      onCreated(await api.initCreate({ folder, ...(name ? { name } : {}) }));
    } catch (err) {
      setError(errorText(err));
    }
  };

  return (
    <form className="card" onSubmit={create} aria-label="New project">
      <h3>New project</h3>
      <label>
        Folder (new or empty)
        <input value={folder} onChange={(e) => setFolder(e.target.value)} placeholder="C:\Users\you\my-addon" required />
      </label>
      <label>
        Name
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Defaults to the folder name" />
      </label>
      {error && <p role="alert">{error}</p>}
      <button type="submit">Create</button>
    </form>
  );
}
