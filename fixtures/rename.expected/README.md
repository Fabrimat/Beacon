# fixtures/rename

Exit fixture for phase 4 criterion 1 ("renaming an entity id updates all references").
`fixtures/rename.expected/` is this tree after `beacon rename demo:car demo:truck`; the CLI
test copies this project to a temp dir, renames, and compares byte for byte.

`demo:car` appears as:

- the BP entity (`src/vehicles/entities/car.entity.json`, JSONC): the definition;
- the RP client entity and the spawn rule: references (never definitions);
- lang keys `entity.demo:car.name` and `item.spawn_egg.entity.demo:car.name` (UTF-8 text);
- a Molang string (`q.is_name_any('demo:car')`);
- `summon demo:car` and `@e[type=demo:car]` in another module (`src/world`);
- a script literal `"demo:car"` and a template head `` `${ns}:car` `` (`scripts/lib/ids.ts`);
- a literal in a generator (`spawner.gen.ts`).
- three literals in a `.mcfunction` template (`src/world/functions/convoy.mcfunction`, with
  `src/world/scope.json`): scanned through the expansion, edited once per source literal, not once
  per loop iteration.

`demo:car_key` and `demo:car_parked` are other names and stay. `demo:lamp` is a block and an
item under one name (renamed together). `demo:horn` is referenced from the static output
`vendor`, so renaming it is `RenameBlocked` and writes nothing.

## Ceilings

- Template literals: only a literal head is recognised, `` `${ns}:<name>` `` or
  `` `${namespace}:<name>` ``, resolved with the file's namespace (its output's `namespace`,
  else the project's). Computed ids (`` `${ns}:${x}` ``, `` `${ns}:car_${i}` ``) are not
  seen; a generator id that does not appear as one literal in its generator is not
  renameable (`RenameBlocked` names the generator).
- Lang keys: the id is cut out by the prefix table (`entity.<id>.name`,
  `item.spawn_egg.entity.<id>.name`, `item.<id>`, `item.<id>.name`, `tile.<id>.name`); an id
  containing `.` is ambiguous with `item.<id>` vs `item.<id>.name`.
- Comments, JSON strings with escape sequences inside an id and ids written in uppercase are not
  scanned. File names are not renamed (`car.entity.json` keeps its name).
