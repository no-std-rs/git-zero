// Deterministic mixed push/history-fetch schedule adapted from PR #47.
export const SEED = 0x484f5354494e4731n;

export function schedule(generations = 32) {
  if (!Number.isInteger(generations) || generations < 4 || generations > 32 || generations % 4) throw new Error("generations must be a multiple of four between 4 and 32");
  let state = SEED;
  const choose = (limit) => {
    state = BigInt.asUintN(64, state ^ (state << 13n));
    state ^= state >> 7n;
    state = BigInt.asUintN(64, state ^ (state << 17n));
    return Number(state % BigInt(limit));
  };
  const result = [];
  for (let generation = 1; generation <= generations; generation++) {
    const have = generation > 1 ? generation - 1 : null;
    result.push({ kind: "push", group: generation === 1 || generation % 4 === 1 ? "large_push" : "small_push", generation, want: generation, have, ref: "refs/heads/main" });
    result.push({ kind: "fetch", group: "latest_fetch", generation, want: generation, have, ref: "refs/heads/main" });
    if (generation % 4 !== 0) continue;
    const fresh = 1 + choose(generation - 1);
    const candidates = Array.from({ length: generation - 2 }, (_, index) => index + 2).filter((value) => value !== fresh);
    const seeded = candidates[choose(candidates.length)], ancestor = 1 + choose(seeded - 1);
    for (const [group, want, have] of [["history_no_haves", fresh, null], ["history_with_haves", seeded, ancestor], ["latest_recheck", generation, generation - 1]]) {
      result.push({ kind: "fetch", group, generation, want, have, ref: group === "latest_recheck" ? "refs/heads/main" : "refs/heads/history-" + want });
    }
  }
  return result;
}
