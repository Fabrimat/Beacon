import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { About } from "../src/screens/About";

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

it("shows versions, channel and that updates are off, with no links", async () => {
  invoke.mockResolvedValue({
    version: "0.1.0-alpha.1",
    engine: "0.1.0-cli",
    protocol: 1,
    platform: "windows-x86_64",
  });
  const { container } = render(<About />);
  expect(await screen.findByText(/Beacon 0\.1\.0-alpha\.1/)).toBeTruthy();
  expect(screen.getByText(/beacon CLI 0\.1\.0-cli/)).toBeTruthy();
  expect(screen.getByText("Channel: alpha")).toBeTruthy();
  expect(screen.getByText("Updates are off in this build")).toBeTruthy();
  expect(screen.getByText(/ask the person who gave you this installer/)).toBeTruthy();
  expect(container.querySelector("a")).toBeNull();
  expect(invoke).not.toHaveBeenCalledWith("update_check");
});
