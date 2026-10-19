// A fake beacon: prints canned JSON. FAKE=hang|garbage|noapi|uninstalled changes the behaviour.
const mode = process.env.FAKE ?? "";
const a = process.argv.slice(2).join(" ");
const arg = (long, takesValue = false) => ({ long, short: null, takesValue });
const help = {
  schema: 1,
  version: "0.0.0",
  command: {
    name: "beacon",
    args: [arg("json"), arg("project", true), arg("yes")],
    commands: [
      { name: "build", args: [arg("target", true), arg("all"), arg("trust")], commands: [] },
      { name: "check", args: [arg("full")], commands: [] },
      { name: "plugin", args: [], commands: [{ name: "update", args: [arg("yes")], commands: [] }] },
    ],
  },
};
const symbols = {
  schema: 1,
  symbols: [
    { kind: "entity", name: "demo:car", definitions: [], references: 1 },
    { kind: "lang_key", name: "entity.demo:car.name", definitions: [], references: 0 },
  ],
};
const api = { schema: 1, gameVersion: "1.26.50", installed: true, modules: { "@minecraft/server": ["2.10.0", "2.11.0-beta"] }, vanilla: { entity: 129 } };
// lookup: exact keys first; module-agnostic like the real one
const paths = {
  "Player": ["@minecraft/server@2.10.0#Player"],
  "Player.name": ["@minecraft/server@2.10.0#Player.name"],
  "World": ["@minecraft/ui-other@1.0.0#World"],
  "minecraft:zombie": ["vanilla:entity#minecraft:zombie", "vanilla:block#minecraft:zombie_head"],
  "minecraft:health": ["component:entity#minecraft:health", "vanilla:effect#minecraft:health_boost"],
};
if (mode === "hang") setTimeout(() => {}, 60_000);
else if (mode === "garbage") console.log("{not json");
else if (a === "--help --json") console.log(JSON.stringify(help));
else if (a === "symbols --json") console.log(JSON.stringify(symbols));
else if (a === "api --json") {
  if (mode === "noapi") { console.error("error: unrecognized subcommand 'api'"); process.exit(2); }
  console.log(JSON.stringify(mode === "uninstalled" ? { schema: 1, gameVersion: null, installed: false, modules: {}, vanilla: {} } : api));
} else if (process.argv[2] === "api" && process.argv[3] === "lookup" && process.argv[5] === "--json") {
  if (mode === "uninstalled" || mode === "noapi") process.exit(1);
  console.log(JSON.stringify({ schema: 1, gameVersion: "1.26.50", hits: (paths[process.argv[4]] ?? []).map((path) => ({ path, signature: null, doc: null, deprecated: false })) }));
} else process.exit(64);
