// Shared helpers imported by _map.ts files (relative import; no _map.ts here, so not a module).
export function colorScopes(colors: string[]): { color: string }[] {
	return colors.map((color) => ({ color }));
}

export const LOOT_DIR: string = "BP/loot_tables/";
