import { generate, item } from "beacon:runtime";

// Heavy: 2,000 items, each with lang + texture-registry strings built by concatenation.
const N = 2000;
const lang: string[] = [];
const textures: Record<string, { textures: string }> = {};

for (let i = 0; i < N; i++) {
  const key = `gem_${String(i).padStart(4, "0")}`;
  const title = key.split("_").map((w) => w[0].toUpperCase() + w.slice(1)).join(" ");
  lang.push(`item.demo:${key}.name=${title}`);
  textures[key] = { textures: `textures/items/${key}` };
  item(`demo:${key}`, {
    format_version: "1.21.0",
    "minecraft:item": {
      description: { identifier: `demo:${key}`, menu_category: { category: "items" } },
      components: { "minecraft:icon": key, "minecraft:max_stack_size": 64 },
    },
  });
}

generate({ kind: "lang", id: "en_US", text: lang.join("\n") });
generate({ kind: "registry", id: "item_texture", data: { resource_pack_name: "demo", texture_data: textures } });
