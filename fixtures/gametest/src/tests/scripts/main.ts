// `beacon test` fixture. `gt:empty` is src/tests/structures/empty.mcstructure: a 1x1x1 air
// structure (the project namespace `gt` prefixes routed structures).
import { register, registerAsync } from "@minecraft/server-gametest";

register("beacon", "passes", (test) => {
  test.succeed();
})
  .structureName("gt:empty")
  .maxTicks(40);

registerAsync("beacon", "fails", async (test) => {
  await test.idle(2);
  test.fail("expected a pig");
})
  .structureName("gt:empty")
  .maxTicks(40);

// never succeeds: the game fails it after maxTicks (100 ticks = 5 s), a timeout to Beacon
register("beacon", "hangs", () => {})
  .structureName("gt:empty")
  .maxTicks(100);
