import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { TrustPrompt } from "../src/screens/TrustPrompt";

const key = [{ legacyFilter: { id: "a\u001b" } }];
const prompt = {
  root: "C:/p",
  runs: ["Plugin `legacyFilter`: runs arbitrary code", "<img src=x onerror=alert(1)>"],
  plugins: [{ legacyFilter: { id: "a" } }],
  key,
};

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

describe("TrustPrompt", () => {
  it("renders project text as text and grants exactly the key it was shown", async () => {
    invoke.mockImplementation(async (cmd: string) => (cmd === "trust_prompt" ? prompt : "trusted"));
    const done = vi.fn();
    const { container } = render(<TrustPrompt root="C:/p" onDone={done} onCancel={() => {}} />);
    await screen.findByText("<img src=x onerror=alert(1)>");
    expect(container.querySelector("img")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: /trust and continue/i }));
    await waitFor(() => expect(done).toHaveBeenCalledWith("trusted"));
    expect(invoke).toHaveBeenCalledWith("trust_grant", { root: "C:/p", shown: key });
  });

  it("shows the prompt again when the project changed", async () => {
    invoke.mockImplementation(async (cmd: string) => (cmd === "trust_prompt" ? prompt : "untrusted"));
    const done = vi.fn();
    render(<TrustPrompt root="C:/p" onDone={done} onCancel={() => {}} />);
    await userEvent.click(await screen.findByRole("button", { name: /trust and continue/i }));
    await screen.findByText(/project changed/i);
    expect(done).not.toHaveBeenCalled();
    expect(invoke.mock.calls.filter(([c]) => c === "trust_prompt")).toHaveLength(2);
  });
});
