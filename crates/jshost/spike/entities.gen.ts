import { entity } from "beacon:runtime";
import template from "./entity.template.json";

type Json = { [k: string]: any };

function merge(a: Json, b: Json): Json {
  const out: Json = { ...a };
  for (const [k, v] of Object.entries(b)) {
    out[k] = v && typeof v === "object" && !Array.isArray(v) ? merge(a[k] ?? {}, v) : v;
  }
  return out;
}

// N entities from a data list, each deep-merged onto a shared template.
const N = 500;
const list = Array.from({ length: N }, (_, i) => ({
  id: `demo:mob_${i}`,
  health: 10 + (i % 40),
  speed: 0.1 + (i % 10) / 20,
  tags: [`tier_${i % 5}`, "generated"],
}));

for (const d of list) {
  entity(d.id, merge(template, {
    "minecraft:entity": {
      description: { identifier: d.id },
      components: {
        "minecraft:health": { value: d.health, max: d.health },
        "minecraft:movement": { value: d.speed },
        "minecraft:type_family": { family: d.tags },
      },
    },
  }));
}
