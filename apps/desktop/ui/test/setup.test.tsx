import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));

import { Home } from "../src/screens/Home";
import { Setup } from "../src/screens/Setup";

const step = (tool: string, status: string) => ({
  tool,
  version: "1",
  bytes: 1048576,
  status,
  defaultSelected: tool !== "bds",
  needsEula: tool === "bds",
});
const report = (installs: unknown[] = [{ product: "bedrock", layout: "gdk", comMojang: "C:/mc" }]) => ({
  schema: 1,
  installs,
  steps: [step("esbuild", "installed"), step("apiIndex", "missing"), step("bds", "missing")],
});
const mockSetup = (r = report()) =>
  invoke.mockImplementation(async (cmd: string) => (cmd === "setup_status" || cmd === "setup_run" ? r : null));

afterEach(() => {
  cleanup();
  invoke.mockReset();
  localStorage.clear();
});

const openOptional = async () => userEvent.click(await screen.findByText(/optional tools/i));

describe("Setup", () => {
  it("leaves BDS unchecked and gates it on the EULA checkbox", async () => {
    mockSetup();
    render(<Setup onDone={() => {}} />);
    await openOptional();
    const bds = (await screen.findByRole("checkbox", { name: /bedrock dedicated server/i })) as HTMLInputElement;
    expect(bds.checked).toBe(false);
    expect(bds.disabled).toBe(true);
    await userEvent.click(screen.getByRole("checkbox", { name: /minecraft eula/i }));
    expect(bds.disabled).toBe(false);
    await userEvent.click(bds);
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("setup_run", { accepted: ["apiIndex", "bds"], acceptEula: true }),
    );
  });

  it("declining everything still reaches Home without installing", async () => {
    mockSetup();
    const done = vi.fn();
    render(<Setup onDone={done} />);
    await openOptional();
    await userEvent.click(await screen.findByRole("checkbox", { name: /api index/i }));
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(done).toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalledWith("setup_run", expect.anything());
  });

  it("shows the BCN0200 fix text when no Minecraft is found", async () => {
    mockSetup(report([]));
    render(<Setup onDone={() => {}} />);
    expect((await screen.findByRole("alert")).textContent).toMatch(/BCN0200.*Start Minecraft once/);
  });
});

describe("Home", () => {
  it("links to Play, Settings and About", () => {
    render(<Home project={null} onProject={() => {}} />);
    for (const [name, href] of [
      ["Play", "#play"],
      ["Settings", "#settings"],
      ["About", "#about"],
    ])
      expect(screen.getByRole("link", { name }).getAttribute("href")).toBe(href);
  });
});
