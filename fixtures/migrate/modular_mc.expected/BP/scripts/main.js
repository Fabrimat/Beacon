// packs/data/modular_mc/a_core/scripts/main.ts
import { world } from "@minecraft/server";
var CORE_ID = "mmc:core";
world.afterEvents.worldLoad.subscribe(() => {
  world.sendMessage(`[${CORE_ID}] loaded`);
});

// packs/data/modular_mc/b_extra/scripts/extra.ts
import { system } from "@minecraft/server";
system.runInterval(() => {
}, 20);
