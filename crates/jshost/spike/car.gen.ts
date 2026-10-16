import { entity } from "beacon:runtime";

entity("demo:car", {
  "minecraft:entity": {
    description: { identifier: "demo:car", is_spawnable: true },
    components: { "minecraft:health": { value: 40 } },
  },
});
