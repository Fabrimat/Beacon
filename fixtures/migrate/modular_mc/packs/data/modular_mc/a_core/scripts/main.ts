import { world } from "@minecraft/server";

export const CORE_ID: string = "mmc:core";

world.afterEvents.worldLoad.subscribe(() => {
	world.sendMessage(`[${CORE_ID}] loaded`);
});
