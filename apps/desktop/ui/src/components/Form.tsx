// A small JSON Schema form: strings, enums, booleans, integers, arrays of strings, nested
// objects and string/number maps. Just the subset Beacon's own schemas use. Every string
// (labels, descriptions, values) is rendered as React text, never as HTML.

export interface Schema {
  type?: string | string[];
  enum?: unknown[];
  properties?: Record<string, Schema>;
  additionalProperties?: boolean | Schema;
  items?: Schema;
  description?: string;
  default?: unknown;
}

type Json = unknown;
const types = (s: Schema) => (Array.isArray(s.type) ? s.type : s.type ? [s.type] : []);
const isObj = (v: Json): v is Record<string, Json> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/** `onChange(undefined)` means "unset": the parent drops the key. */
interface FieldProps {
  name: string;
  schema: Schema;
  value: Json;
  onChange: (v: Json) => void;
}

const Hint = ({ s }: { s: Schema }) =>
  s.description ? <small className="muted"> {s.description}</small> : null;

function Field({ name, schema, value, onChange }: FieldProps) {
  const ts = types(schema);
  if (schema.enum) {
    return (
      <label>
        {name}
        <Hint s={schema} />
        <select
          value={value === undefined || value === null ? "" : String(value)}
          onChange={(e) => {
            const o = schema.enum!.find((x) => String(x) === e.target.value);
            onChange(e.target.value === "" ? undefined : o);
          }}
        >
          <option value="">(not set)</option>
          {schema.enum.filter((o) => o !== null).map((o) => (
            <option key={String(o)} value={String(o)}>
              {String(o)}
            </option>
          ))}
        </select>
      </label>
    );
  }
  if (ts.includes("boolean")) {
    return (
      <label className="check">
        <input
          type="checkbox"
          checked={typeof value === "boolean" ? value : schema.default === true}
          onChange={(e) => onChange(e.target.checked)}
        />
        {name}
        <Hint s={schema} />
      </label>
    );
  }
  if (ts.includes("string") || ts.includes("integer")) {
    const num = ts.includes("integer") && !ts.includes("string");
    return (
      <label>
        {name}
        <Hint s={schema} />
        <input
          type={num ? "number" : "text"}
          value={value === undefined || value === null ? "" : String(value)}
          onChange={(e) => {
            const t = e.target.value;
            onChange(t === "" ? undefined : num ? Number(t) : t);
          }}
        />
      </label>
    );
  }
  if (ts.includes("array") && schema.items) {
    const list = Array.isArray(value) ? value : [];
    return (
      <fieldset>
        <legend>
          {name}
          <Hint s={schema} />
        </legend>
        {list.map((v, i) => (
          <div className="row" key={i}>
            <Field
              name={`${name} ${i + 1}`}
              schema={schema.items!}
              value={v}
              onChange={(x) => onChange(list.map((y, j) => (j === i ? (x ?? "") : y)))}
            />
            <button
              type="button"
              className="secondary"
              aria-label={`Remove ${name} ${i + 1}`}
              onClick={() => onChange(list.filter((_, j) => j !== i))}
            >
              Remove
            </button>
          </div>
        ))}
        <button type="button" className="secondary" onClick={() => onChange([...list, ""])}>
          Add {name}
        </button>
      </fieldset>
    );
  }
  if (ts.includes("object")) {
    const obj = isObj(value) ? value : {};
    const set = (k: string, v: Json) => {
      const next = { ...obj };
      if (v === undefined) delete next[k];
      else next[k] = v;
      onChange(next);
    };
    const props = Object.entries(schema.properties ?? {});
    const extra = schema.additionalProperties;
    return (
      <fieldset>
        <legend>
          {name}
          <Hint s={schema} />
        </legend>
        {props.map(([k, s]) => (
          <Field key={k} name={k} schema={s} value={obj[k]} onChange={(v) => set(k, v)} />
        ))}
        {isObj(extra) && <MapRows name={name} item={extra} obj={obj} onChange={onChange} />}
      </fieldset>
    );
  }
  return (
    <p className="muted">
      {name}: not editable here
    </p>
  );
}

/** Free-form keys (module.json `paths`, `hooks.tick`, ...): one row per entry. */
function MapRows({
  name,
  item,
  obj,
  onChange,
}: {
  name: string;
  item: Schema;
  obj: Record<string, Json>;
  onChange: (v: Json) => void;
}) {
  const rows = Object.entries(obj);
  const blank = types(item).includes("integer") ? 0 : "";
  return (
    <>
      {rows.map(([k, v], i) => (
        <div className="row" key={i}>
          <label>
            {`${name} key ${i + 1}`}
            <input
              type="text"
              value={k}
              onChange={(e) =>
                // rename in place: the entry keeps its position
                onChange(Object.fromEntries(rows.map(([a, b], j) => [j === i ? e.target.value : a, b])))
              }
            />
          </label>
          <Field
            name={`${name} value ${i + 1}`}
            schema={item}
            value={v}
            onChange={(x) => onChange({ ...obj, [k]: x ?? blank })}
          />
          <button
            type="button"
            className="secondary"
            aria-label={`Remove ${name} ${i + 1}`}
            onClick={() => onChange(Object.fromEntries(rows.filter((_, j) => j !== i)))}
          >
            Remove
          </button>
        </div>
      ))}
      <button
        type="button"
        className="secondary"
        // "" is a legal key to type into; two blank rows would collapse, so one at a time
        disabled={"" in obj}
        onClick={() => onChange({ ...obj, "": blank })}
      >
        Add {name} entry
      </button>
    </>
  );
}

export function Form({
  schema,
  value,
  onChange,
}: {
  schema: Schema;
  value: Json;
  onChange: (v: Json) => void;
}) {
  const obj = isObj(value) ? value : {};
  return (
    <div className="form">
      {Object.entries(schema.properties ?? {}).map(([k, s]) => (
        <Field
          key={k}
          name={k}
          schema={s}
          value={obj[k]}
          onChange={(v) => {
            const next = { ...obj };
            if (v === undefined) delete next[k];
            else next[k] = v;
            onChange(next);
          }}
        />
      ))}
    </div>
  );
}
