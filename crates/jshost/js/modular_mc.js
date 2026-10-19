// Port of ModularMC's evaluation (map-ts.ts, json-template.ts, text-template.ts, json-merge.ts,
// auto-map-resolver.ts) into one driver for Beacon's QuickJS host. MIT, (c) Nusiq; see NOTICE.
// Differences from upstream are only the ones the host forces: the output tree is virtual
// (nothing is written), sources are read through `__legacy` (VFS-backed, read-only), and
// every path is project-relative. Entry state lives in `globalThis.__mmc`.
(() => {
  const H = globalThis.__legacy;
  delete globalThis.__legacy;
  const ENV = Object.assign(Object.create(null), JSON.parse(globalThis.__env));
  delete globalThis.__env;
  const logs = [];
  const envRead = {};
  const modules = [];
  const out = new Map();
  // Regolith copies packs/BP, packs/RP and data into its tmp dir first: seed them (no contributors)
  for (const [path, src] of JSON.parse(globalThis.__seed)) out.set(path, { kind: "pass", text: undefined, src, contribs: [] });
  delete globalThis.__seed;
  let autoMap = null;
  let autoMapWarned = false;

  class MmcError extends Error {}

  // ---- shims
  const fmt = (a) => {
    if (typeof a === "string") return a;
    try { return JSON.stringify(a) ?? String(a); } catch { return String(a); }
  };
  const log = (...a) => { logs.push(a.map(fmt).join(" ")); };
  globalThis.console = { log, info: log, warn: log, error: log, debug: log };
  const seen = (k) => {
    if (typeof k !== "string") return undefined;
    const v = Object.hasOwn(ENV, k) ? ENV[k] : undefined;
    envRead[k] = v === undefined ? null : v;
    return v;
  };
  globalThis.process = {
    env: new Proxy({}, { get: (_, k) => seen(k), has: (_, k) => seen(k) !== undefined }),
  };

  // ---- paths (posix, project-relative)
  const norm = (p) => {
    const o = [];
    for (const s of p.split("/")) {
      if (s === "" || s === ".") continue;
      if (s === "..") { if (o.length && o[o.length - 1] !== "..") o.pop(); else o.push(".."); }
      else o.push(s);
    }
    return o.join("/");
  };
  const join = (...p) => norm(p.join("/"));
  const trim = (p) => p.replace(/\/+$/, "");
  const dirname = (p) => { p = trim(p); const i = p.lastIndexOf("/"); return i < 0 ? "." : i === 0 ? "/" : p.slice(0, i); };
  const basename = (p) => { p = trim(p); return p.slice(p.lastIndexOf("/") + 1); };
  const extname = (p) => { const b = basename(p), i = b.lastIndexOf("."); return i <= 0 ? "" : b.slice(i); };

  // ---- JSONC (comments and trailing commas), string-aware
  const jsonc = (text) => {
    let o = "", i = 0;
    const n = text.length;
    while (i < n) {
      const c = text[i];
      if (c === '"') {
        let j = i + 1;
        while (j < n && text[j] !== '"') j += text[j] === "\\" ? 2 : 1;
        o += text.slice(i, j + 1); i = j + 1;
      } else if (c === "/" && text[i + 1] === "/") {
        while (i < n && text[i] !== "\n") i++;
      } else if (c === "/" && text[i + 1] === "*") {
        const j = text.indexOf("*/", i + 2);
        i = j < 0 ? n : j + 2;
      } else { o += c; i++; }
    }
    // trailing commas: a comma followed (after whitespace) by } or ]
    let r = "";
    for (let k = 0, inStr = false; k < o.length; k++) {
      const c = o[k];
      if (inStr) { r += c; if (c === "\\") r += o[++k] ?? ""; else if (c === '"') inStr = false; continue; }
      if (c === '"') { inStr = true; r += c; continue; }
      if (c === ",") { const m = /^\s*[}\]]/.exec(o.slice(k + 1)); if (m) continue; }
      r += c;
    }
    return JSON.parse(r.replace(/^﻿/, ""));
  };

  // ---- json-merge.ts (ListMergePolicy.APPEND only: the only policy map-ts uses)
  const mergeObjects = (a, b) => {
    if (typeof a !== typeof b) return b;
    if (a !== null && b !== null && typeof a === "object") {
      if (Array.isArray(a) && Array.isArray(b)) return [...a, ...b];
      if (!Array.isArray(a) && !Array.isArray(b)) return mergeDict(a, b);
    }
    return b;
  };
  const mergeDict = (a, b) => {
    const r = {}, used = new Set();
    for (const k of [...Object.keys(a), ...Object.keys(b)]) {
      if (used.has(k)) continue;
      used.add(k);
      if (k in b) r[k] = k in a ? mergeObjects(a[k], b[k]) : b[k];
      else if (k in a) r[k] = a[k];
    }
    return r;
  };

  // ---- json-template.ts
  class K {
    constructor(identifier, scope = {}) {
      if (typeof identifier !== "string") {
        throw new MmcError(`K class identifier must be a string\nType: ${typeof identifier}\nValue: ${JSON.stringify(identifier)}`);
      }
      this.identifier = identifier; this.scope = scope;
    }
  }
  class JoinStr { constructor(separator) { this.separator = separator; } }
  const k = (id, scope = {}) => new K(id, scope);
  const joinStr = (s) => new JoinStr(s);
  const isExpr = (v) => typeof v === "string" && v.startsWith("::");
  const evalExpr = (expr, scope) => {
    const fn = new Function("K", "joinStr", ...Object.keys(scope), `return ${expr};`);
    try { return fn(k, joinStr, ...Object.values(scope)); }
    catch (e) { throw new MmcError(`Failed to evaluate expression.\nExpression: ${expr}\n${e}`); }
  };
  const unpackOf = (item, scope) => {
    if (isExpr(item.__unpack__)) {
      const v = evalExpr(item.__unpack__.slice(2), scope);
      if (!Array.isArray(v)) throw new MmcError(`__unpack__ expression must evaluate to an array\nType: ${typeof v}\nValue: ${JSON.stringify(v)}`);
      return v;
    }
    if (Array.isArray(item.__unpack__)) return item.__unpack__;
    throw new MmcError(`__unpack__ must be an array or a ::-prefixed expression that evaluates to an array,\nType: ${typeof item.__unpack__}\nValue: ${JSON.stringify(item.__unpack__)}`);
  };
  const checkScope = (s) => {
    if (typeof s !== "object" || s === null) throw new MmcError(`Each item in __unpack__ must be an object to use as scope,\nType: ${typeof s}\nValue: ${JSON.stringify(s)}`);
  };
  const evaluate = (t, scope = {}) => {
    if (t === null || t === undefined) return t;
    if (Array.isArray(t)) {
      const result = [];
      for (const item of t) {
        if (typeof item === "object" && item !== null && "__unpack__" in item) {
          for (const us of unpackOf(item, scope)) {
            checkScope(us);
            const ms = { ...scope, ...us };
            let v;
            if ("__value__" in item) {
              v = isExpr(item.__value__) ? evalExpr(item.__value__.slice(2), ms) : evaluate(item.__value__, ms);
            } else {
              const { __unpack__, ...rest } = item;
              v = evaluate(rest, ms);
            }
            if (v !== undefined) result.push(v);
          }
        } else {
          const v = evaluate(item, scope);
          if (v !== undefined) result.push(v);
        }
      }
      if (result.length > 0 && result[0] instanceof JoinStr) {
        return result.slice(1).map((i) => String(i)).join(result[0].separator);
      }
      return result;
    }
    if (typeof t === "object") {
      const result = {};
      for (const [key, value] of Object.entries(t)) {
        if (!isExpr(key)) {
          const v = evaluate(value, scope);
          if (v !== undefined) result[key] = v;
          continue;
        }
        const ek = evalExpr(key.slice(2), scope);
        if (ek === undefined) continue;
        if (ek instanceof K) {
          const v = evaluate(value, { ...scope, ...ek.scope });
          if (v !== undefined) result[ek.identifier] = v;
        } else if (Array.isArray(ek)) {
          if (ek.every((i) => i instanceof K)) {
            for (const kk of ek) {
              const v = evaluate(value, { ...scope, ...kk.scope });
              if (v !== undefined) result[kk.identifier] = v;
            }
          } else if (ek.every((i) => typeof i === "string")) {
            for (const kk of ek) {
              const v = evaluate(value, scope);
              if (v !== undefined) result[kk] = v;
            }
          } else {
            throw new MmcError(`Array keys must contain only K instances or strings.\nType: ${typeof ek}\nKey: ${key}\nEvaluated key: ${JSON.stringify(ek)}`);
          }
        } else {
          if (typeof ek !== "string") throw new MmcError(`Object keys must be strings.\nType: ${typeof ek}\nKey: ${key}\nEvaluated key: ${JSON.stringify(ek)}`);
          const v = evaluate(value, scope);
          if (v !== undefined) result[ek] = v;
        }
      }
      return result;
    }
    if (isExpr(t)) return evalExpr(t.slice(2), scope);
    return t;
  };

  // ---- text-template.ts
  const evalText = (src, scope) => src.replace(/{ts:(.+?):}/gs, (_, expr) => {
    const e = expr.trim();
    const body = H.countStatements(e) > 1 || e.startsWith("return ") ? e : `return ${e};`;
    const fn = new Function(...Object.keys(scope), body);
    try { return String(fn(...Object.values(scope))); }
    catch (err) { throw new MmcError(`Error evaluating TypeScript expression:\n${e}\n\nError: ${err}`); }
  });

  // ---- auto-map-resolver.ts
  const resolveAuto = (sourcePath, flat) => {
    const filename = basename(sourcePath);
    const pattern = Object.keys(autoMap).find((p) => filename.endsWith(p));
    if (pattern === undefined) return null;
    const m = autoMap[pattern];
    const dir = typeof m === "string" ? m : m.path;
    const ext = typeof m === "string" ? "" : m.extension || "";
    const base = filename.slice(0, filename.length - pattern.length) + (ext || pattern);
    if (!flat) {
      const sd = dirname(sourcePath);
      if (sd && sd !== "." && sd !== "./") return join(dir, sd.replace(/^\.\//g, "").replace(/\/\.\//g, "/"), base);
    }
    return join(dir, base);
  };

  // ---- globs: the Deno expandGlobSync subset (`*` stops at `/`, `**`, `?`), files only, sorted
  const globRe = (g) => {
    let r = "";
    for (let i = 0; i < g.length; i++) {
      const c = g[i];
      if (c === "*" && g[i + 1] === "*") {
        if (g[i + 2] === "/") { r += "(?:.*/)?"; i += 2; } else { r += ".*"; i++; }
      } else if (c === "*") r += "[^/]*";
      else if (c === "?") r += "[^/]";
      else if (c === "[" || c === "{") throw new MmcError(`glob syntax \`${c}\` is not supported by migrate: ${g}`);
      else r += c.replace(/[.+^$()|\\\]}]/g, "\\$&");
    }
    return new RegExp(`^${r}$`);
  };
  // the real path for `p` when each segment has exactly one case-insensitive match, else null
  const caseFix = (p) => {
    let at = "";
    for (const seg of p.split("/")) {
      const names = H.listDir(at).map((n) => n.replace(/\/$/, ""));
      const hit = names.includes(seg) ? [seg] : names.filter((n) => n.toLowerCase() === seg.toLowerCase());
      if (hit.length !== 1) return null;
      at = at ? `${at}/${hit[0]}` : hit[0];
    }
    return H.exists(at) ? at : null;
  };
  const expandGlob = (dir, pattern) => {
    const re = globRe(pattern.replace(/^\.\//, ""));
    const depth = pattern.includes("**") ? Infinity : pattern.split("/").length;
    const found = [];
    const walk = (rel, d) => {
      for (const n of H.listDir(join(dir, rel))) {
        const isDir = n.endsWith("/"), name = isDir ? n.slice(0, -1) : n;
        const r = rel ? `${rel}/${name}` : name;
        if (isDir) { if (d < depth) walk(r, d + 1); }
        else if (re.test(r)) found.push(r);
      }
    };
    walk("", 1);
    return found.sort();
  };

  // ---- map-ts.ts: MapTsEntry.validate / fromObject
  const bad = (msg, map) => new MmcError(`${msg}\nFile: ${map}`);
  const validate = (o, map) => {
    if (typeof o !== "object" || o === null || !("source" in o) || !("target" in o) || typeof o.source !== "string") {
      throw bad("Invalid MAP entry. Each entry must have a 'source' (string) and a 'target' (string or object).", map);
    }
    const { source, target, jsonTemplate, textTemplate, onConflict, fileType, scope } = o;
    if (typeof target !== "string" && typeof target !== "object") throw bad("Invalid MAP entry. Target must be a string or an object.", map);
    if (typeof target === "object") {
      if (target === null || !("path" in target) || !("name" in target)) throw bad("Invalid MAP entry. Target object must have 'path' and 'name' properties.", map);
      if (typeof target.path !== "string" || typeof target.name !== "string") throw bad("Invalid MAP entry. Target 'path' and 'name' must be strings.", map);
      if (target.subpath !== undefined && typeof target.subpath !== "string") throw bad("Invalid MAP entry. Target 'subpath' must be a string.", map);
    }
    if (jsonTemplate !== undefined && typeof jsonTemplate !== "boolean") throw bad("Invalid jsonTemplate property. jsonTemplate must be a boolean.", map);
    if (textTemplate !== undefined && typeof textTemplate !== "boolean") throw bad("Invalid textTemplate property. textTemplate must be a boolean.", map);
    if (jsonTemplate === true && textTemplate === true) throw bad("Invalid configuration. jsonTemplate and textTemplate cannot both be true.", map);
    if (onConflict !== undefined) {
      const ok = ["stop", "skip", "merge", "overwrite", "appendStart", "appendEnd"];
      if (typeof onConflict !== "string") throw bad("Invalid onConflict property. onConflict must be a string.", map);
      if (!ok.includes(onConflict)) throw bad(`Invalid onConflict value.\nAccepted values: ${ok.join(", ")}`, map);
    }
    if (fileType !== undefined && typeof fileType !== "string") throw bad("Invalid fileType property. fileType must be a string.", map);
    if (scope !== undefined && typeof scope !== "object") throw bad("Invalid scope property. scope must be an object.", map);
    if (typeof target === "string" && target !== ":auto" && target !== ":autoFlat") {
      const t = norm(target);
      if (!t.startsWith("RP/") && !t.startsWith("BP/") && !t.startsWith("data/")) {
        throw bad(`Invalid target path. Target must start with RP/, BP/, or data/.\nGot: ${t}`, map);
      }
    }
    if (source.startsWith("/") || /^[A-Za-z]:/.test(source)) throw bad(`Absolute source paths are not supported by migrate.\nGot: ${source}`, map);
    if (norm(source).includes("..")) throw bad(`Invalid source path. Paths cannot contain parent directory references (..).\nGot: ${source}`, map);
    return o;
  };
  const expandEntries = (raw, map) => {
    const o = validate(raw, map), dir = dirname(map);
    const mk = (src) => ({
      source: src, target: o.target, jsonTemplate: o.jsonTemplate || false, textTemplate: o.textTemplate || false,
      onConflict: o.onConflict || "stop", fileType: o.fileType, scope: o.scope || {},
    });
    if (/[*?[{]/.test(o.source)) {
      const files = expandGlob(dir, o.source).map((f) => mk(join(dir, f)));
      if (files.length === 0) log(`Warning: Glob pattern '${o.source}' in ${map} matched no files`);
      return files;
    }
    return [mk(join(dir, o.source))];
  };

  // ---- map-ts.ts: MapTsEntry.resolveTargetPath / apply
  const targetOf = (e, map) => {
    const rel = e.source.slice(dirname(map).length + 1);
    if (!autoMap) {
      if (!autoMapWarned) { autoMapWarned = true; log("Warning: Failed to load AUTO_MAP: auto-map.ts not found"); }
      autoMap = {};
    }
    const auto = (flat, what) => {
      const r = resolveAuto(rel, flat);
      if (!r) throw bad(`Failed to auto-map${what}.\nSource path: ${rel}`, map);
      return r;
    };
    const t = e.target;
    if (typeof t === "string") {
      if (t === ":auto" || t === ":autoFlat") return auto(t === ":autoFlat", "");
      if (t.endsWith("/") || ["..", "."].includes(basename(t))) return join(t, basename(e.source));
      return t;
    }
    let p = t.path;
    if (p === ":auto" || p === ":autoFlat") p = dirname(auto(p === ":autoFlat", " path"));
    let name = t.name;
    if (name === ":auto") name = basename(auto(false, " name"));
    return join(p, t.subpath || "", name);
  };
  const ftype = (p, o) => o || extname(p).slice(1).toLowerCase();
  const mergeable = (a, b) => ["json", "material"].includes(a) && ["json", "material"].includes(b);
  // lazy: seeded base files are only read when something merges into or appends to them
  const textOf = (o) => (o.text === undefined ? (o.text = H.readText(o.src) ?? null) : o.text);
  const scopeJson = (s) => { try { return JSON.parse(JSON.stringify(s)); } catch { return {}; } };

  const applyEntry = (e, idx, map) => {
    const target = norm(targetOf(e, map));
    if (target === "" || target.startsWith("..")) throw bad(`Target escapes the project: ${target}`, map);
    const cur = out.get(target);
    if (!cur && [...out.keys()].some((p) => p.startsWith(`${target}/`))) {
      throw bad(`Target path is a directory and cannot be overwritten.\nPath: ${target}`, map);
    }
    if (!H.exists(e.source)) {
      // Windows/macOS file systems are case-insensitive, so the real filter found these
      const fixed = caseFix(e.source);
      if (!fixed) throw new MmcError(`Missing file: ${e.source.slice(dirname(map).length + 1)}`);
      log(`Warning: ${map}: source ${e.source} only matches ${fixed} case-insensitively (breaks on Linux)`);
      e.source = fixed;
    }
    const src = H.readText(e.source) ?? null; // null = not UTF-8 (a binary copy)
    const st = ftype(e.source, e.fileType), tt = ftype(target, e.fileType);
    const text = (what) => { if (src === null) throw bad(`${what}: source is not UTF-8 text: ${e.source}`, map); return src; };
    const noJson = () => {
      if (mergeable(st, tt)) throw bad(`Cannot use "${e.onConflict}" with JSON files. JSON files can be merged using "merge" option.`, map);
    };
    const parse = (s, f) => { try { return jsonc(s); } catch (err) { throw bad(`Failed to parse JSON.\nFile: ${f}\n${err}`, map); } };
    const rec = (applied) => {
      const o = out.get(target);
      o.contribs.push({
        map, entry: idx, source: e.source, policy: e.onConflict, applied,
        template: e.jsonTemplate ? { kind: "json", scope: scopeJson(e.scope) }
          : e.textTemplate ? { kind: "text", scope: scopeJson(e.scope) } : { kind: "none" },
      });
    };
    const put = (kind, body, extra) => {
      const o = cur ?? { contribs: [] };
      Object.assign(o, { kind, text: body, src: null }, extra);
      out.set(target, o);
    };
    const prev = cur ? textOf(cur) : null;
    const eol = prev?.endsWith("\r\n") ? "\r\n" : "\n";
    if (!cur || e.onConflict === "overwrite") {
      if (e.textTemplate) { noJson(); put("text", evalText(text("textTemplate"), e.scope)); }
      else if (e.jsonTemplate) put("json", JSON.stringify(evaluate(parse(text("jsonTemplate"), e.source), e.scope), null, "\t"));
      else put("pass", src, { src: e.source });
      rec(cur ? "overwrote" : "created");
    } else if (e.onConflict === "appendStart" || e.onConflict === "appendEnd") {
      noJson();
      if (prev === null) throw bad("Cannot append to a binary target.", map);
      const s = e.textTemplate ? evalText(text("textTemplate"), e.scope) : text("append");
      if (e.onConflict === "appendStart") {
        put("text", s + (s.length > 0 && prev.length > 0 && !s.endsWith("\n") && !prev.startsWith("\n") ? eol : "") + prev);
      } else {
        put("text", prev + (prev.length > 0 && s.length > 0 && !prev.endsWith("\n") && !s.startsWith("\n") ? eol : "") + s);
      }
      rec("appended");
    } else if (e.onConflict === "merge") {
      if (!mergeable(st, tt)) throw bad(`Cannot merge files with types because of incompatible types.\nSource type: ${st}\nTarget type: ${tt}\nTypes that can be merged: json, material`, map);
      let sj = parse(text("merge"), e.source);
      if (e.jsonTemplate) sj = evaluate(sj, e.scope);
      if (prev === null) throw bad("Cannot merge into a binary target.", map);
      put("json", JSON.stringify(mergeObjects(parse(prev, target), sj), null, "\t"));
      rec("merged");
    } else if (e.onConflict === "stop") {
      throw bad(`Target file already exists. Use onConflict: "skip", "merge", "overwrite", "appendStart", or "appendEnd" to handle this.\nPath: ${target}`, map);
    } else {
      log(`Skipped exporting ${e.source} to ${target}. Target already exists.`);
      rec("skipped");
    }
  };

  // ---- driver API (called from Rust)
  const dump = (error) => ({
    error,
    outputs: [...out].filter(([, o]) => o.contribs.length).map(([path, o]) => ({ path, kind: o.kind, text: o.text, src: o.src, contributors: o.contribs })),
    scripts: modules.flatMap((m) => m.scripts.map((s) => join(dirname(m.path), s))),
    envRead, logs,
  });
  globalThis.__mmc = {
    // `__ns` = the evaluated module namespace of a `_map.ts` (upstream MapTs.fromFile)
    add(path, ns) {
      const entries = [], scripts = [];
      if (ns.MAP !== undefined) {
        if (!Array.isArray(ns.MAP)) throw bad("MAP must be an array.", path);
        ns.MAP.forEach((raw, i) => { for (const e of expandEntries(raw, path)) { e.idx = i; entries.push(e); } });
      }
      if (ns.SCRIPTS !== undefined) {
        if (!Array.isArray(ns.SCRIPTS)) throw bad("SCRIPTS must be an array.", path);
        for (const s of ns.SCRIPTS) {
          if (typeof s !== "string") throw bad("Each script in SCRIPTS must be a string.", path);
          scripts.push(s);
        }
      }
      if (entries.length === 0 && scripts.length === 0) log(`Warning: ${path} does not export either MAP or SCRIPTS`);
      modules.push({ path, entries, scripts });
    },
    autoMap(_path, ns) {
      if (!ns.AUTO_MAP) throw new Error("AUTO_MAP not found");
      autoMap = ns.AUTO_MAP;
    },
    run() {
      for (const m of modules) {
        try {
          // MapTs.apply: "concurrent" entries (onConflict "stop", no jsonTemplate) run first
          // (Promise.all), then the rest in order
          const conc = (e) => e.onConflict === "stop" && !e.jsonTemplate;
          for (const e of m.entries.filter(conc)) applyEntry(e, e.idx, m.path);
          for (const e of m.entries.filter((e) => !conc(e))) applyEntry(e, e.idx, m.path);
        } catch (err) {
          return dump({ map: m.path, message: `Failed to apply MAP entry.\n${err && err.message ? err.message : err}` });
        }
      }
      return dump(null);
    },
    dump: (error) => JSON.stringify(dump(error)),
  };
  const run = globalThis.__mmc.run;
  globalThis.__mmc.run = () => JSON.stringify(run());
})();
