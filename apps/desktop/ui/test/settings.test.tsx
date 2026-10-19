import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import type { ProjectSummary } from "../src/api";
import { Settings } from "../src/screens/Settings";

// the real schema the engine ships
// @ts-expect-error the UI has no @types/node; vite's fs.strict also denies ?raw outside the app
const { readFileSync } = await import("node:fs");
// vitest runs from apps/desktop/ui
const schema = JSON.parse(readFileSync("../../../crates/engine/schemas/module.schema.json", "utf8"));
const project: ProjectSummary = {
  root: "C:/p",
  name: "p",
  trust: "trusted",
  targets: ["dev"],
  playTarget: "dev",
};
const file = { kind: "module", module: "src/main" };
const doc = (value: Record<string, unknown>, base: string) => ({ file, value, schema, base });

/** settings_list -> one module; settings_read -> the queued docs in order. */
const backend = (reads: unknown[], write?: () => Promise<unknown>) =>
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === "settings_list") return ["src/main"];
    if (cmd === "settings_read") return reads.shift();
    if (cmd === "settings_write") return write!();
  });

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

describe("Settings", () => {
  it("renders a form from the schema and submits the base hash", async () => {
    backend([doc({ output: "main", paths: { "ui/*": "RP/ui" } }, "h1")], async () =>
      doc({ output: "main", paths: { "ui/*": "RP/ui" }, autoTextures: false }, "h2"),
    );
    render(<Settings project={project} />);
    await userEvent.click(await screen.findByRole("button", { name: "src/main" }));
    expect(((await screen.findByLabelText(/^output/)) as HTMLInputElement).value).toBe("main");
    expect(screen.getByLabelText(/^onConflict/).tagName).toBe("SELECT");
    expect(screen.getByLabelText("paths key 1")).toBeTruthy();
    const box = screen.getByLabelText(/^autoTextures/) as HTMLInputElement;
    expect(box.checked).toBe(true); // the schema default
    await userEvent.click(box);
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("settings_write", {
        root: "C:/p",
        file,
        base: "h1",
        value: { output: "main", paths: { "ui/*": "RP/ui" }, autoTextures: false },
      }),
    );
    expect(await screen.findByText("Saved.")).toBeTruthy();
    // the next save quotes the new base
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("settings_write", expect.objectContaining({ base: "h2" })),
    );
  });

  it("a stale save re-reads the file and says so", async () => {
    backend([doc({ output: "a" }, "h1"), doc({ output: "theirs" }, "h9")], async () => {
      throw { code: "BCN0164", message: "changed on disk" };
    });
    render(<Settings project={project} />);
    await userEvent.click(await screen.findByRole("button", { name: "src/main" }));
    const out = (await screen.findByLabelText(/^output/)) as HTMLInputElement;
    await userEvent.type(out, "x");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(/changed on disk since you opened it/)).toBeTruthy();
    expect(((await screen.findByLabelText(/^output/)) as HTMLInputElement).value).toBe("theirs");
  });

  it("shows other errors with their code and renders file text as text", async () => {
    backend([doc({ output: "<img src=x onerror=alert(1)>" }, "h1")], async () => {
      throw { code: "BCN0163", message: "open in an editor" };
    });
    const { container } = render(<Settings project={project} />);
    await userEvent.click(await screen.findByRole("button", { name: "src/main" }));
    await screen.findByLabelText(/^output/);
    expect(container.querySelector("img")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect((await screen.findByRole("alert")).textContent).toBe("BCN0163: open in an editor");
  });
});
