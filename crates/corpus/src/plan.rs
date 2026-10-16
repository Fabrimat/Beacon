//! Pure, write-free description of a corpus. Everything is derived from (size, seed).
use crate::Size;
use serde_json::{Value, json};

/// splitmix64: tiny, fast, identical on every platform.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Inclusive range.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.next() as usize % xs.len()]
    }
}

pub enum Body {
    Text(String),
    /// Magic header followed by pseudo-random (incompressible) bytes; never parsed.
    Bin {
        magic: &'static [u8],
        len: u64,
        seed: u64,
    },
}

pub struct File {
    pub path: String,
    pub body: Body,
}

pub struct Plan {
    pub files: Vec<File>,
    pub modules: usize,
    pub generators: usize,
}

pub struct Stats {
    pub files: usize,
    pub bytes: u64,
    pub binary_bytes: u64,
    pub loc: usize,
    pub modules: usize,
    pub generators: usize,
}

impl Plan {
    pub fn stats(&self) -> Stats {
        let (mut bytes, mut binary_bytes, mut loc) = (0, 0, 0);
        for f in &self.files {
            match &f.body {
                Body::Text(t) => {
                    bytes += t.len() as u64;
                    if f.path.contains("/scripts/") && f.path.ends_with(".ts") {
                        loc += t.bytes().filter(|&b| b == b'\n').count();
                    }
                }
                Body::Bin { len, .. } => {
                    bytes += len;
                    binary_bytes += len;
                }
            }
        }
        Stats {
            files: self.files.len(),
            bytes,
            binary_bytes,
            loc,
            modules: self.modules,
            generators: self.generators,
        }
    }
}

const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const OGG: &[u8] = b"OggS";
const ADJ: &[&str] = &[
    "ruby", "iron", "ember", "frost", "jade", "storm", "shadow", "gilded", "ancient", "wild",
];
const NOUN: &[&str] = &[
    "sword", "pickaxe", "crystal", "totem", "lantern", "golem", "wisp", "beast", "idol", "bow",
];

/// Per-module structure, decided before any sizes so totals can be spread exactly.
struct Shape {
    items: u64,
    blocks: u64,
    ents: u64,
    fns: u64,
    sound_gen: bool,
    data_gen: u64, // entries in a *.data.json driving a *.gen.ts; 0 = none
    module_json: bool,
    png_w: Vec<u64>,
    ogg_w: Vec<u64>,
    ts_w: Vec<u64>,
}

impl Shape {
    fn files(&self) -> usize {
        let n = 2 * self.items + 2 * self.blocks + 5 * self.ents + self.fns + 1;
        let n = n as usize + self.ogg_w.len() + self.ts_w.len();
        n + self.sound_gen as usize + 2 * (self.data_gen > 0) as usize + self.module_json as usize
    }
}

pub fn plan(size: Size, seed: u64) -> Plan {
    let div = match size {
        Size::Large => 1,
        Size::Medium => 4,
        Size::Small => 20,
    };
    let (target_files, bin_bytes, loc) = (3000 / div, 60_000_000 / div as u64, 20_000 / div);
    let mut rng = Rng::new(seed);

    // Pass 1: shapes until the file target is reached.
    let (mut shapes, mut total) = (Vec::new(), 0);
    while total < target_files {
        let i = shapes.len();
        let (items, blocks, ents) = (rng.range(2, 4), rng.range(1, 3), rng.range(1, 3));
        let oggs = rng.range(2, 4);
        let tss = rng.range(1, 3);
        let s = Shape {
            items,
            blocks,
            ents,
            fns: rng.range(1, 3),
            sound_gen: i % 14 == 3,
            data_gen: if i % 14 == 10 { rng.range(3, 8) } else { 0 },
            module_json: i % 10 == 4,
            png_w: (0..items + blocks + ents)
                .map(|_| rng.range(50, 150))
                .collect(),
            ogg_w: (0..oggs).map(|_| rng.range(50, 150) * 20).collect(),
            ts_w: (0..tss).map(|_| rng.range(50, 150)).collect(),
        };
        total += s.files();
        shapes.push(s);
    }
    let sum = |f: fn(&Shape) -> &Vec<u64>| shapes.iter().flat_map(|s| f(s).iter()).sum::<u64>();
    let bin_w = sum(|s| &s.png_w) + sum(|s| &s.ogg_w);
    let ts_sum = sum(|s| &s.ts_w);

    // Pass 2: content.
    let mut files: Vec<File> = Vec::new();
    let mut generators = 0;
    let multi_output = !matches!(size, Size::Small);
    let mut add = |path: String, body: Body| files.push(File { path, body });
    let text = |v: Value| Body::Text(serde_json::to_string_pretty(&v).expect("json") + "\n");
    for (mi, s) in shapes.iter().enumerate() {
        let lib = multi_output && mi % 5 == 0;
        let m = format!("{}_{mi:03}", if lib { "lib" } else { "mod" });
        let ns = if lib { "lib" } else { "demo" };
        let base = format!("src/{m}");
        let mut pngs = s.png_w.iter();
        let mut png = |add: &mut dyn FnMut(String, Body), path: String, rng: &mut Rng| {
            let w = pngs.next().expect("png weight");
            add(
                path,
                Body::Bin {
                    magic: PNG,
                    len: bin_bytes * w / bin_w,
                    seed: rng.next(),
                },
            );
        };
        let mut lang = String::new();
        let mut k = 0;
        let mut name = |rng: &mut Rng| {
            k += 1;
            format!("{}_{}_{mi}_{k}", rng.pick(ADJ), rng.pick(NOUN))
        };

        for _ in 0..s.items {
            let n = name(&mut rng);
            let id = format!("{ns}:{n}");
            lang += &format!("item.{id}.name={n}\n");
            add(
                format!("{base}/items/{n}.item.json"),
                text(json!({
                    "format_version": "1.21.100",
                    "minecraft:item": {
                        "description": {"identifier": id, "menu_category": {"category": "equipment"}},
                        "components": {
                            "minecraft:max_stack_size": 1,
                            "minecraft:icon": {"textures": {"default": n}},
                            "minecraft:damage": rng.range(2, 12),
                            "minecraft:durability": {"max_durability": rng.range(50, 900)}
                        }
                    }
                })),
            );
            png(&mut add, format!("{base}/items/{n}.png"), &mut rng);
        }
        for _ in 0..s.blocks {
            let n = name(&mut rng);
            let id = format!("{ns}:{n}");
            lang += &format!("tile.{id}.name={n}\n");
            add(
                format!("{base}/blocks/{n}.block.json"),
                text(json!({
                    "format_version": "1.21.100",
                    "minecraft:block": {
                        "description": {"identifier": id, "menu_category": {"category": "construction"}},
                        "components": {
                            "minecraft:destructible_by_mining": {"seconds_to_destroy": rng.range(1, 20) as f64 / 2.0},
                            "minecraft:light_emission": rng.range(0, 15),
                            "minecraft:material_instances": {"*": {"texture": n, "render_method": "opaque"}}
                        }
                    }
                })),
            );
            png(&mut add, format!("{base}/blocks/{n}.png"), &mut rng);
        }
        for _ in 0..s.ents {
            let n = name(&mut rng);
            let id = format!("{ns}:{n}");
            lang += &format!("entity.{id}.name={n}\n");
            add(
                format!("{base}/entities/{n}.entity.json"),
                text(entity_bp(&id, &mut rng)),
            );
            add(
                format!("{base}/entities/{n}.client_entity.json"),
                text(json!({
                    "format_version": "1.10.0",
                    "minecraft:client_entity": {"description": {
                        "identifier": id,
                        "materials": {"default": "entity_alpha"},
                        "textures": {"default": format!("textures/entity/{n}")},
                        "geometry": {"default": format!("geometry.{n}")},
                        "animations": {"walk": format!("animation.{n}.walk")},
                        "scripts": {"animate": ["walk"]},
                        "render_controllers": ["controller.render.default"]
                    }}
                })),
            );
            add(
                format!("{base}/entities/{n}.geo.json"),
                text(json!({
                    "format_version": "1.12.0",
                    "minecraft:geometry": [{
                        "description": {"identifier": format!("geometry.{n}"), "texture_width": 64, "texture_height": 64,
                            "visible_bounds_width": 2, "visible_bounds_height": 2, "visible_bounds_offset": [0, 1, 0]},
                        "bones": [{"name": "body", "pivot": [0, 0, 0],
                            "cubes": [{"origin": [-4, 0, -4], "size": [8, rng.range(6, 16), 8], "uv": [0, 0]}]}]
                    }]
                })),
            );
            add(
                format!("{base}/entities/{n}.animation.json"),
                text(json!({
                    "format_version": "1.8.0",
                    "animations": {format!("animation.{n}.walk"): {
                        "loop": true, "animation_length": 1.0,
                        "bones": {"body": {"rotation": {"0.0": [0, 0, 0], "0.5": [0, rng.range(5, 25), 0], "1.0": [0, 0, 0]}}}
                    }}
                })),
            );
            png(&mut add, format!("{base}/entities/{n}.png"), &mut rng);
        }
        for (o, w) in s.ogg_w.iter().enumerate() {
            add(
                format!("{base}/sounds/{}_{o}.ogg", rng.pick(NOUN)),
                Body::Bin {
                    magic: OGG,
                    len: bin_bytes * w / bin_w,
                    seed: rng.next(),
                },
            );
        }
        for f in 0..s.fns {
            let mut t = String::new();
            for _ in 0..rng.range(4, 12) {
                t += &match rng.next() % 3 {
                    0 => format!("say {} spawned\n", rng.pick(NOUN)),
                    1 => format!("scoreboard players add @s {ns}_score {}\n", rng.range(1, 9)),
                    _ => format!(
                        "effect @s speed {} {} true\n",
                        rng.range(5, 60),
                        rng.range(0, 3)
                    ),
                };
            }
            add(format!("{base}/functions/fn_{f}.mcfunction"), Body::Text(t));
        }
        add(
            format!("{base}/texts/en_US.lang"),
            Body::Text(format!("pack.{m}.name={m}\n{lang}")),
        );
        for (si, w) in s.ts_w.iter().enumerate() {
            let sname = ["handlers", "events", "ui"][si];
            let lines = (loc as u64 * w / ts_sum) as usize;
            add(
                format!("{base}/scripts/{sname}.ts"),
                Body::Text(script(ns, &m, sname, lines, &mut rng)),
            );
        }
        if s.sound_gen {
            generators += 1;
            add(
                format!("{base}/sounds/sounds.gen.ts"),
                Body::Text(
                    "export default generate(({ assets, ns }) =>\n  assets(\"./*.ogg\").map(f => sound(`${ns}:${f.name}`, { file: f.path })));\n".into(),
                ),
            );
        }
        if s.data_gen > 0 {
            generators += 1;
            let list: Vec<Value> = (0..s.data_gen)
                .map(|e| json!({"id": format!("{}_{mi}_{e}", rng.pick(NOUN)), "health": rng.range(5, 60)}))
                .collect();
            add(
                format!("{base}/data/mobs.data.json"),
                text(Value::Array(list)),
            );
            add(
                format!("{base}/data/mobs.gen.ts"),
                Body::Text(
                    "import mobs from \"./mobs.data.json\";\n\nexport default generate(({ ns }) =>\n  mobs.map(m => entity(`${ns}:${m.id}`, {\n    server: { components: { \"minecraft:health\": { value: m.health } } },\n  })));\n".into(),
                ),
            );
        }
        if s.module_json {
            let mut mj = json!({"onConflict": "merge", "paths": {"ui/*.json": "RP/ui"}});
            if multi_output {
                mj["output"] = json!(if lib { "lib" } else { "main" });
            }
            add(format!("{base}/module.json"), text(mj));
        }
    }

    let mut beacon = json!({"version": "0.4.0", "sources": ["./src"], "versionPolicy": "auto"});
    if multi_output {
        beacon["outputs"] = json!([
            {"id": "main", "include": ["src/mod_*/**"]},
            {"id": "lib", "include": ["src/lib_*/**"], "namespace": "lib"}
        ]);
    }
    add(
        "config.json".into(),
        text(json!({
            "type": "minecraftBedrock",
            "name": "beacon-corpus",
            "authors": ["Beacon"],
            "targetVersion": "1.21.100",
            "namespace": "demo",
            "packs": {"behaviorPack": "./packs/BP", "resourcePack": "./packs/RP"},
            "beacon": beacon
        })),
    );
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Plan {
        files,
        modules: shapes.len(),
        generators,
    }
}

fn entity_bp(id: &str, rng: &mut Rng) -> Value {
    json!({
        "format_version": "1.21.100",
        "minecraft:entity": {
            "description": {"identifier": id, "is_spawnable": true, "is_summonable": true},
            "components": {
                "minecraft:health": {"value": rng.range(5, 60), "max": 60},
                "minecraft:movement": {"value": rng.range(10, 40) as f64 / 100.0},
                "minecraft:collision_box": {"width": 0.8, "height": 1.6},
                "minecraft:physics": {}
            }
        }
    })
}

/// A game-script file with exactly `loc` lines (comment padding on the last few).
fn script(ns: &str, m: &str, file: &str, loc: usize, rng: &mut Rng) -> String {
    let mut lines: Vec<String> = vec![
        "import { Player, system, world } from \"@minecraft/server\";".into(),
        String::new(),
    ];
    let mut i = 0;
    while lines.len() + 14 <= loc {
        i += 1;
        let (noun, adj) = (rng.pick(NOUN), rng.pick(ADJ));
        let (a, b) = (rng.range(1, 9), rng.range(10, 99));
        lines.extend([
            format!(
                "export function {file}{}{i}(player: Player, amount: number): number {{",
                cap(noun)
            ),
            format!("  const key = \"{ns}:{m}_{adj}_{i}\";"),
            "  const current = (player.getDynamicProperty(key) as number | undefined) ?? 0;".into(),
            format!("  const next = Math.min(current + amount * {a}, {b});"),
            "  if (next === current) {".into(),
            format!("    player.sendMessage(`{adj} {noun} is already at its limit`);"),
            "    return current;".into(),
            "  }".into(),
            "  player.setDynamicProperty(key, next);".into(),
            format!(
                "  system.runTimeout(() => world.sendMessage(`${{player.name}} reached ${{next}}`), {b});"
            ),
            "  return next;".into(),
            "}".into(),
            String::new(),
            format!("// {m}/{file}: {adj} {noun}"),
        ]);
    }
    while lines.len() < loc {
        lines.push(format!("// {m} {file} end"));
    }
    lines.join("\n") + "\n"
}

fn cap(s: &str) -> String {
    s[..1].to_uppercase() + &s[1..]
}
