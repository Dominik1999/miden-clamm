import { describe, it, expect } from "vitest";
import { parseDeployment, POOL_SLOTS } from "@/lib/clamm/deployment";

const valid = {
  network: { rpcUrl: "http://localhost:57291", proverUrl: "http://localhost:50051" },
  pool: { id: "0x9e54030f993b3311620ba6a47f6e2f", feePips: 3000, tickSpacing: 60, initialTick: 0 },
  token0: { id: "0xbe7384179a6bd43176aae2ef7e20d6", symbol: "TKA", decimals: 6, devSecretKeyHex: "aabb" },
  token1: { id: "0xa499f7c830ca55517ae8d824651849", symbol: "TKB", decimals: 6 },
  roots: { swap: "0x1", mint: "0x2", burn: "0x3", collect: "0x4", p2id: "0x5" },
};

describe("parseDeployment", () => {
  it("accepts a valid descriptor", () => {
    const d = parseDeployment(valid);
    expect(d.pool.id).toBe(valid.pool.id);
    expect(d.pool.feePips).toBe(3000);
    expect(d.token0.symbol).toBe("TKA");
    expect(d.token0.devSecretKeyHex).toBe("aabb");
    expect(d.token1.devSecretKeyHex).toBeUndefined();
  });

  it("rejects non-objects", () => {
    expect(() => parseDeployment(null)).toThrow(/not an object/);
    expect(() => parseDeployment("json")).toThrow(/not an object/);
  });

  it("rejects a missing or malformed network section", () => {
    expect(() => parseDeployment({ ...valid, network: {} })).toThrow(/rpcUrl/);
  });

  it("rejects invalid account ids", () => {
    expect(() =>
      parseDeployment({ ...valid, pool: { ...valid.pool, id: "0x1234" } }),
    ).toThrow(/pool\.id/);
    expect(() =>
      parseDeployment({ ...valid, token0: { ...valid.token0, id: "nope" } }),
    ).toThrow(/token0\.id/);
  });

  it("rejects invalid pool parameters", () => {
    expect(() =>
      parseDeployment({ ...valid, pool: { ...valid.pool, tickSpacing: 0 } }),
    ).toThrow(/parameters invalid/);
  });

  it("rejects missing roots", () => {
    expect(() =>
      parseDeployment({ ...valid, roots: { swap: "0x1" } }),
    ).toThrow(/roots\.mint/);
  });
});

describe("POOL_SLOTS", () => {
  it("uses the clamm_pool component slot names", () => {
    expect(POOL_SLOTS.sqrtPrice).toBe("clamm_pool::clamm_pool::sqrt_price");
    expect(POOL_SLOTS.poolState).toBe("clamm_pool::clamm_pool::pool_state");
    expect(POOL_SLOTS.liquidity).toBe("clamm_pool::clamm_pool::liquidity");
  });
});
