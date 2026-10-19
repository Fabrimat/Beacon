import { system, world } from "@minecraft/server";

let ticks = 0;
system.runInterval(() => {
  ticks++;
  console.warn(`beacon-e2e tick ${ticks}`); // BREAKPOINT_LINE
  world.sendMessage(`tick ${ticks}`);
}, 20);
