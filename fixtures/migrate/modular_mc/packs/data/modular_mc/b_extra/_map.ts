// Second module: conflict policies against a_core, a glob source, an env branch.
const EDU: boolean = process.env.FIXTURE_EDITION === "education";

export const MAP = [
	// Glob source, one entry per match: BP/items/ruby.item.json, BP/items/sapphire.item.json
	{ source: "glob/*.item.json", target: ":autoFlat" },
	// skip: a_core already wrote BP/items/shared.item.json, this one is dropped
	{ source: "shared.item.json", target: ":autoFlat", onConflict: "skip" },
	// merge: deep-merged into a_core's RP/textures/item_texture.json
	{ source: "item_texture.json", target: "RP/textures/item_texture.json", onConflict: "merge" },
	// overwrite: replaces a_core's BP/recipes/golem.json
	{ source: "golem.recipe.json", target: "BP/recipes/golem.json", onConflict: "overwrite" },
	// appendEnd: appended to a_core's RP/texts/en_US.lang
	{ source: "en_US.lang", target: ":auto", onConflict: "appendEnd" },
	// process.env branch: picks one of two sources for the same target
	{
		source: EDU ? "variants/edition.education.json" : "variants/edition.bedrock.json",
		target: "BP/edition.json",
	},
];

export const SCRIPTS = ["scripts/extra.ts"];
