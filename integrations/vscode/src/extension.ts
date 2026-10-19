import * as vscode from "vscode";
import { execFile } from "node:child_process";
import { LanguageClient, LanguageClientOptions, ServerOptions, TransportKind } from "vscode-languageclient/node";
import { Config, cliArgs, isBeaconManaged, merge, taskArgs } from "./debugConfig";

const TRUST_KEY = "beacon.trusted"; // workspaceState: the user accepted the Beacon trust prompt for this workspace
let client: LanguageClient | undefined;

// `beacon.path` is machine-scoped and a restricted configuration: a repository cannot choose
// which executable runs. Nothing here runs in Restricted Mode (`vscode.workspace.isTrusted`).
const beaconPath = () => vscode.workspace.getConfiguration("beacon").get<string>("path", "beacon");
const folderOf = () => vscode.workspace.workspaceFolders?.[0];
const restricted = () => {
  if (vscode.workspace.isTrusted) return false;
  void vscode.window.showWarningMessage("Beacon is disabled in Restricted Mode. Trust this workspace to use it.");
  return true;
};

export async function activate(ctx: vscode.ExtensionContext): Promise<void> {
  // Beacon's own project trust (generators / plugins), on top of VS Code's workspace trust.
  const trusted = () => ctx.workspaceState.get<boolean>(TRUST_KEY, false) && vscode.workspace.isTrusted;
  // `--trust` only after the user said yes; `--allow` is never passed from here.
  const trust = () => trusted();

  const askTrust = async (): Promise<void> => {
    if (trusted() || !vscode.workspace.isTrusted) return;
    const yes = "Trust";
    const pick = await vscode.window.showWarningMessage(
      "Beacon can run this project's generators and plugins. Only trust projects you got from a source you trust.",
      yes,
      "Not now",
    );
    if (pick === yes) await ctx.workspaceState.update(TRUST_KEY, true);
  };

  const startClient = async () => {
    const folder = folderOf();
    if (!folder || !vscode.workspace.isTrusted || client) return;
    await askTrust();
    const run = {
      command: beaconPath(),
      args: ["serve", "--stdio", "--lsp", "--project", folder.uri.fsPath, ...(trust() ? ["--trust"] : [])],
      transport: TransportKind.stdio,
    };
    const server: ServerOptions = { run, debug: run };
    const opts: LanguageClientOptions = {
      documentSelector: [
        { scheme: "file", language: "json" },
        { scheme: "file", language: "jsonc" },
        { scheme: "file", language: "typescript" },
        { scheme: "file", language: "mcfunction" },
        { scheme: "file", pattern: "**/*.lang" },
      ],
    };
    client = new LanguageClient("beacon", "Beacon", server, opts);
    await client.start();
  };
  const start = () =>
    startClient().catch((e) => {
      client = undefined;
      void vscode.window.showErrorMessage(`Beacon: language server failed to start: ${e}`);
    });

  // No shell: the executable and an argv array (ProcessExecution), target names validated.
  const task = (name: string, cmd: "build" | "watch", target?: unknown) => {
    if (restricted()) return;
    const folder = folderOf();
    if (!folder) return vscode.window.showErrorMessage("Beacon: open a project folder first.");
    let args: string[];
    try {
      args = taskArgs(cmd, folder.uri.fsPath, trust(), target);
    } catch (e) {
      return vscode.window.showErrorMessage(`Beacon: ${(e as Error).message}`);
    }
    const t = new vscode.Task({ type: "beacon", command: cmd }, folder, name, "beacon", new vscode.ProcessExecution(beaconPath(), args), "$tsc");
    t.isBackground = cmd === "watch";
    return vscode.tasks.executeTask(t);
  };

  ctx.subscriptions.push(
    vscode.commands.registerCommand("beacon.build", () => task("Beacon: Build", "build")),
    vscode.commands.registerCommand("beacon.watch", () => task("Beacon: Watch", "watch")),
    vscode.commands.registerCommand("beacon.deploy", () =>
      task("Beacon: Deploy", "build", vscode.workspace.getConfiguration("beacon").get<string>("devTarget", "dev")),
    ),
    vscode.commands.registerCommand("beacon.trust", async () => {
      if (restricted()) return;
      await ctx.workspaceState.update(TRUST_KEY, false);
      await askTrust();
      await client?.stop();
      client = undefined;
      await start();
    }),
    vscode.workspace.onDidGrantWorkspaceTrust(() => void start()),
    // A second provider for the official extension's type: it only touches Beacon-managed entries.
    vscode.debug.registerDebugConfigurationProvider("minecraft-js", {
      async resolveDebugConfiguration(folder, config) {
        if (!isBeaconManaged(config)) return config;
        if (restricted()) return undefined;
        const root = (folder ?? folderOf())?.uri.fsPath;
        if (!root) return undefined;
        try {
          const out = await new Promise<string>((res, rej) =>
            execFile(beaconPath(), cliArgs(config, root), { maxBuffer: 1 << 20 }, (e, so, se) => (e ? rej(new Error(se || so || e.message)) : res(so))),
          );
          return merge(config, JSON.parse(out) as Config) as vscode.DebugConfiguration;
        } catch (e) {
          void vscode.window.showErrorMessage(`Beacon: cannot resolve the debug configuration: ${(e as Error).message}`);
          return undefined; // abort: the official fallback would bind no breakpoints
        }
      },
    }),
  );

  await start();
}

export const deactivate = () => client?.stop();
