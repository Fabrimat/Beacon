import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { type AppError, type ProjectSummary, errorText } from "../api";
import { Form, type Schema } from "../components/Form";

interface Doc {
  file: { kind: "module"; module: string };
  value: Record<string, unknown>;
  schema: Schema;
  /** blake3 of the bytes read; the write must quote it. */
  base: string;
}

const STALE = "BCN0164";
const file = (module: string) => ({ kind: "module", module });

/** `module.json` forms: pick a module, edit, save. The save quotes the base hash; if the file
 *  changed meanwhile (stale) it is re-read, the edits are dropped and the user is told. */
export function Settings({ project }: { project: ProjectSummary | null }) {
  const [modules, setModules] = useState<string[] | null>(null);
  const [module, setModule] = useState<string | null>(null);
  const [doc, setDoc] = useState<Doc | null>(null);
  const [value, setValue] = useState<unknown>({});
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const root = project?.root;

  useEffect(() => {
    if (!root) return;
    invoke<string[]>("settings_list", { root })
      .then(setModules)
      .catch((e) => setError(errorText(e)));
  }, [root]);

  if (!project) return <p className="muted">Open a project on Home first.</p>;

  const load = async (m: string) => {
    const d = await invoke<Doc>("settings_read", { root: project.root, file: file(m) });
    setDoc(d);
    setValue(d.value);
  };

  const pick = async (m: string) => {
    setModule(m);
    setNotice(null);
    setError(null);
    try {
      await load(m);
    } catch (e) {
      setDoc(null);
      setError(errorText(e));
    }
  };

  const save = async () => {
    if (!doc || !module) return;
    setNotice(null);
    setError(null);
    try {
      const d = await invoke<Doc>("settings_write", {
        root: project.root,
        file: file(module),
        base: doc.base,
        value,
      });
      setDoc(d);
      setValue(d.value);
      setNotice("Saved.");
    } catch (e) {
      if ((e as AppError)?.code === STALE) {
        try {
          await load(module);
          setNotice("This file changed on disk since you opened it. It was reloaded; your edits were not saved.");
        } catch (e2) {
          setError(errorText(e2));
        }
      } else {
        setError(errorText(e));
      }
    }
  };

  return (
    <section>
      <h2>Settings</h2>
      {modules && modules.length === 0 && <p className="muted">This project has no modules yet.</p>}
      <ul className="recent" aria-label="Modules">
        {modules?.map((m) => (
          <li key={m}>
            <button
              type="button"
              className="link"
              aria-current={m === module ? "true" : undefined}
              onClick={() => pick(m)}
            >
              {m}
            </button>
          </li>
        ))}
      </ul>
      {error && <p role="alert">{error}</p>}
      {notice && <p role="status">{notice}</p>}
      {doc && module && (
        <form
          className="card"
          aria-label={`${module} settings`}
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <h3>{module}/module.json</h3>
          <Form schema={doc.schema} value={value} onChange={setValue} />
          <button type="submit">Save</button>
        </form>
      )}
    </section>
  );
}
