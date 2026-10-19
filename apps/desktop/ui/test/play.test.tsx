import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
const handlers = new Map<string, (e: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, h: (e: { payload: unknown }) => void) => {
    handlers.set(name, h);
    return () => handlers.delete(name);
  },
}));

import type { ProjectSummary } from "../src/api";
import { Play } from "../src/screens/Play";

const project: ProjectSummary = {
  root: "C:/p",
  name: "p",
  trust: "trusted",
  targets: ["dev"],
  playTarget: "dev",
};
const diag = (code: string, severity: string, message: string) => ({ code, severity, message });
const report = (diagnostics: unknown[]) => ({ diagnostics, deployedTo: null, launched: false });

afterEach(() => {
  cleanup();
  invoke.mockReset();
  handlers.clear();
});

describe("Play", () => {
  it("shows BCN0200 as the headline with its fix, as text", async () => {
    const msg =
      "No Minecraft install found. Start Minecraft once, sign in and reach the main menu, then retry.";
    invoke.mockResolvedValue(report([diag("BCN0200", "warning", msg)]));
    render(<Play project={project} onProject={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Play" }));
    expect(await screen.findByRole("heading", { name: msg })).toBeTruthy();
    expect(invoke).toHaveBeenCalledWith("play", { root: "C:/p", target: "dev" });
  });

  it("renders the content log as text, never as HTML", async () => {
    invoke.mockResolvedValue(report([]));
    const { container } = render(<Play project={project} onProject={() => {}} />);
    await waitFor(() => expect(handlers.has("beacon://content-log")).toBe(true));
    act(() =>
      handlers.get("beacon://content-log")!({
        payload: { level: "error", text: "<img src=x onerror=alert(1)>" },
      }),
    );
    expect(await screen.findByText("<img src=x onerror=alert(1)>")).toBeTruthy();
    expect(container.querySelector("img")).toBeNull();
  });

  it("routes BCN0050 to the trust prompt", async () => {
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "play"
        ? report([diag("BCN0050", "error", "project has code and is not trusted")])
        : { root: "C:/p", runs: ["Generators and scripts"], plugins: [], key: [] },
    );
    render(<Play project={{ ...project, trust: "untrusted" }} onProject={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Play" }));
    expect(
      await screen.findByRole("heading", { name: "Trust this project to Play" }),
    ).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Trust this project" }));
    expect(await screen.findByText("Generators and scripts")).toBeTruthy();
  });

  it("offers a target picker only with more than one target", () => {
    const { rerender } = render(<Play project={project} onProject={() => {}} />);
    expect(screen.queryByLabelText("Target")).toBeNull();
    rerender(<Play project={{ ...project, targets: ["dev", "edu"] }} onProject={() => {}} />);
    expect(screen.getByLabelText("Target")).toBeTruthy();
  });
});
