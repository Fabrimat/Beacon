// One sound event per .ogg in ./sounds: `minimal:beep` -> sounds/demo/beep.ogg
export default generate(({ assets, ns }) =>
  assets("./sounds/*.ogg").map((f) => sound(`${ns}:${f.name}`, { file: f.path })),
);
