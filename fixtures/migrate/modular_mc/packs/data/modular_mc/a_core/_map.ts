import scope from "../scope.json" with { type: "json" };
import { colorScopes, LOOT_DIR } from "../_shared/lib.ts";

export const MAP = [
	// :auto keeps the sub-folder: BP/entities/sub/golem.behavior.json
	{ source: "sub/golem.behavior.json", target: ":auto" },
	// :autoFlat drops it: RP/entity/golem.entity.json
	{ source: "sub/golem.entity.json", target: ":autoFlat" },
	// Object target with extension replacement: BP/features/extra/tree_custom.json
	{
		source: "sub/tree.feature.json",
		target: { path: ":autoFlat", subpath: "extra", name: "tree_custom.json" },
	},
	// Directory target (trailing slash keeps the file name): BP/loot_tables/golem.loot.json
	{ source: "golem.loot.json", target: LOOT_DIR },
	// jsonTemplate: `::` expressions, K(), __unpack__, scope from a JSON import
	{
		source: "templates/paint.item.json",
		target: "BP/items/paint.json",
		jsonTemplate: true,
		scope: { ...scope, variants: colorScopes(scope.colors) },
	},
	// textTemplate: {ts: ... :}
	{
		source: "templates/setup.mcfunction",
		target: ":auto",
		textTemplate: true,
		scope: { namespace: scope.namespace, health: scope.health },
	},
	// Bases for the conflict pairs in b_extra
	{ source: "shared.item.json", target: ":autoFlat" },
	{ source: "item_texture.json", target: "RP/textures/item_texture.json" },
	{ source: "golem.recipe.json", target: "BP/recipes/golem.json" },
	{ source: "en_US.lang", target: ":auto" },
];

export const SCRIPTS = ["scripts/main.ts"];
