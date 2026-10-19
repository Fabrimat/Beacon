// Typed wrappers over the Tauri commands (beacon_serve::app::COMMANDS). All text from a
// project arrives sanitized (AppCore) and is rendered as React text, never as HTML.
import { invoke } from "@tauri-apps/api/core";

export type TrustState = "trusted" | "untrusted" | "noCode";

export interface ProjectSummary {
  root: string;
  name: string;
  trust: TrustState;
  targets: string[];
  playTarget: string | null;
}

export interface TrustPrompt {
  root: string;
  runs: string[];
  plugins: unknown[];
  /** Opaque: handed back to trust_grant unchanged, never rendered. */
  key: unknown[];
}

export interface AppInfo {
  version: string;
  engine: string;
  protocol: number;
  platform: string;
}

export interface AppError {
  code: string;
  message: string;
}

export const errorText = (e: unknown): string =>
  typeof e === "object" && e !== null && "message" in e
    ? `${(e as AppError).code ?? "error"}: ${(e as AppError).message}`
    : String(e);

export type Tool = "esbuild" | "apiIndex" | "bds";
export type StepStatus = "installed" | "missing" | "declined" | "failed" | "unsupported";

export interface SetupStep {
  tool: Tool;
  version: string;
  bytes: number;
  status: StepStatus;
  defaultSelected: boolean;
  needsEula: boolean;
  diagnostic?: { code: string; message: string };
}

export interface SetupReport {
  schema: number;
  installs: { product: string; layout: string; comMojang: string }[];
  steps: SetupStep[];
}

export const api = {
  setupStatus: () => invoke<SetupReport>("setup_status"),
  setupRun: (accepted: Tool[], acceptEula: boolean) =>
    invoke<SetupReport>("setup_run", { accepted, acceptEula }),
  appInfo: () => invoke<AppInfo>("app_info"),
  projectOpen: (root: string) => invoke<ProjectSummary>("project_open", { root }),
  trustPrompt: (root: string) => invoke<TrustPrompt>("trust_prompt", { root }),
  trustGrant: (root: string, shown: unknown[]) =>
    invoke<TrustState>("trust_grant", { root, shown }),
  initCreate: (answers: Record<string, unknown>) =>
    invoke<ProjectSummary>("init_create", { answers }),
};
