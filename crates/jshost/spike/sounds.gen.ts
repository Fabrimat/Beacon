import { assets, sound } from "beacon:runtime";

// One sound definition per .ogg (ModularMC "sound map" pattern).
for (const path of assets("sounds/**/*.ogg")) {
  const name: string = path.replace(/^sounds\//, "").replace(/\.ogg$/, "");
  sound(`mod.${name.replace(/\//g, ".")}`, {
    category: "neutral",
    max_distance: 32,
    sounds: [{ name: path.replace(/\.ogg$/, ""), volume: 1.0, load_on_low_memory: true }],
  });
}
