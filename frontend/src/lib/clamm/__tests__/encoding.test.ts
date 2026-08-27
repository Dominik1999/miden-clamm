import { describe, it, expect } from "vitest";
import {
  TICK_OFFSET,
  tickToFelt,
  feltToTick,
  swapStorage,
  mintStorage,
  burnStorage,
  collectStorage,
  networkTargetWord,
  p2idStorage,
} from "@/lib/clamm/encoding";
import { loadGolden } from "./golden";

const golden = loadGolden();
const pool = golden.accounts.pool.hex;
const user = golden.accounts.user.hex;

const toBigints = (values: string[]) => values.map((v) => BigInt(v));

describe("tick encoding", () => {
  it("uses the Rust TICK_OFF offset (2^19)", () => {
    expect(TICK_OFFSET).toBe(golden.tickOff);
  });

  it("offset-encodes ticks", () => {
    expect(tickToFelt(0)).toBe(BigInt(TICK_OFFSET));
    expect(tickToFelt(-120)).toBe(BigInt(TICK_OFFSET - 120));
    expect(tickToFelt(120)).toBe(BigInt(TICK_OFFSET + 120));
  });

  it("round-trips", () => {
    for (const tick of [-443_636, -6000, -1, 0, 1, 60, 443_636]) {
      expect(feltToTick(tickToFelt(tick))).toBe(tick);
    }
  });

  it("rejects non-integer ticks", () => {
    expect(() => tickToFelt(1.5)).toThrow(/integer/);
  });
});

describe("swapStorage", () => {
  it("matches the golden Rust swap-note storage layout exactly", () => {
    const goldenSwap = golden.notes.find((n) => n.kind === "swap")!;
    // Golden note: direction 0, min_out 12_345_678_901, recipient user, deadline 4242.
    const storage = swapStorage({
      poolHex: pool,
      direction: 0,
      minOut: 12_345_678_901n,
      recipientHex: user,
      deadline: 4242,
    });
    expect(storage).toEqual(toBigints(goldenSwap.storage));
  });

  it("splits min_out into 32-bit lo/hi limbs", () => {
    const storage = swapStorage({
      poolHex: pool,
      direction: 1,
      minOut: (7n << 32n) | 5n,
      recipientHex: user,
      deadline: 1,
    });
    expect(storage[2]).toBe(1n);
    expect(storage[3]).toBe(5n);
    expect(storage[4]).toBe(7n);
  });

  it("rejects out-of-range min_out", () => {
    expect(() =>
      swapStorage({ poolHex: pool, direction: 0, minOut: 2n ** 64n, recipientHex: user, deadline: 1 }),
    ).toThrow(/u64/);
  });
});

describe("mintStorage", () => {
  it("matches the golden Rust mint-note storage layout exactly", () => {
    const goldenMint = golden.notes.find((n) => n.kind === "mint")!;
    // Golden note: [-120, 120], liquidity 1e12, deadline 4242.
    const storage = mintStorage({
      poolHex: pool,
      tickLower: -120,
      tickUpper: 120,
      liquidity: 1_000_000_000_000n,
      deadline: 4242,
    });
    expect(storage).toEqual(toBigints(goldenMint.storage));
  });
});

describe("burnStorage", () => {
  it("matches the golden Rust burn-note storage layout exactly", () => {
    const goldenBurn = golden.notes.find((n) => n.kind === "burn")!;
    const storage = burnStorage({
      poolHex: pool,
      tickLower: -120,
      tickUpper: 120,
      liquidity: 1_000_000_000_000n,
    });
    expect(storage).toEqual(toBigints(goldenBurn.storage));
  });
});

describe("collectStorage", () => {
  it("matches the golden Rust collect-note storage layout exactly", () => {
    const goldenCollect = golden.notes.find((n) => n.kind === "collect")!;
    const storage = collectStorage({ poolHex: pool, tickLower: -120, tickUpper: 120 });
    expect(storage).toEqual(toBigints(goldenCollect.storage));
  });
});

describe("networkTargetWord", () => {
  it("matches the golden NetworkAccountTarget attachment word", () => {
    const goldenSwap = golden.notes.find((n) => n.kind === "swap")!;
    expect(networkTargetWord(pool)).toEqual(toBigints(goldenSwap.attachmentWord));
  });
});

describe("p2idStorage", () => {
  it("matches the golden P2ID storage felts ([suffix, prefix])", () => {
    expect(p2idStorage(user)).toEqual(toBigints(golden.p2id.storageFelts));
  });
});
