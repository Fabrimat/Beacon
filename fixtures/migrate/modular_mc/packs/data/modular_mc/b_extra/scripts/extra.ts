import { system } from "@minecraft/server";

system.runInterval(() => {
	// extra module tick, bundled after a_core's script (SCRIPTS order)
}, 20);
