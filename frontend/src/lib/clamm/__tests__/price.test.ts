import { describe, it, expect } from "vitest";
import {
  Q96,
  sqrtPriceX96ToPrice,
  formatPrice,
  spotQuote,
  minOutFromSlippage,
  deadlineHeight,
  formatTokenAmount,
  parseTokenAmount,
} from "@/lib/clamm/price";
import { loadGolden } from "./golden";

const golden = loadGolden();
const ratio = (tick: number) => BigInt(golden.sqrtRatios[String(tick)]);

describe("sqrtPriceX96ToPrice", () => {
  it("is exactly 1.0 at tick 0 with equal decimals (sqrtP = 2^96)", () => {
    expect(ratio(0)).toBe(Q96); // sanity: golden tick-0 ratio is exactly 2^96
    expect(sqrtPriceX96ToPrice(ratio(0), 6, 6)).toBe(1);
  });

  it("matches 1.0001^tick against golden TickMath ratios", () => {
    expect(sqrtPriceX96ToPrice(ratio(120), 6, 6)).toBeCloseTo(1.0001 ** 120, 6);
    expect(sqrtPriceX96ToPrice(ratio(-120), 6, 6)).toBeCloseTo(1.0001 ** -120, 6);
    expect(sqrtPriceX96ToPrice(ratio(6000), 6, 6)).toBeCloseTo(1.0001 ** 6000, 4);
    expect(sqrtPriceX96ToPrice(ratio(-6000), 6, 6)).toBeCloseTo(1.0001 ** -6000, 6);
  });

  it("adjusts for decimal differences", () => {
    // token0 has 8 decimals, token1 has 6: raw price 1.0 -> human price 100.
    expect(sqrtPriceX96ToPrice(ratio(0), 8, 6)).toBe(100);
    expect(sqrtPriceX96ToPrice(ratio(0), 6, 8)).toBe(0.01);
  });

  it("returns 0 for a zero price", () => {
    expect(sqrtPriceX96ToPrice(0n, 6, 6)).toBe(0);
  });
});

describe("formatPrice", () => {
  it("adapts precision to magnitude", () => {
    expect(formatPrice(1234.5678)).toBe("1234.57");
    expect(formatPrice(1.23456789)).toBe("1.2346");
    expect(formatPrice(0.00123456)).toBe("0.001235");
    expect(formatPrice(0.00000012)).toBe("1.2000e-7");
    expect(formatPrice(0)).toBe("0");
    expect(formatPrice(Number.NaN)).toBe("0");
  });
});

describe("spotQuote", () => {
  it("quotes exactly at tick 0 (P = 1): fee is the only deduction", () => {
    // At sqrtP = 2^96 the price is exactly 1, so out = in * (1e6 - fee)/1e6.
    expect(
      spotQuote({ amountIn: 1_000_000n, sqrtPriceX96: Q96, zeroForOne: true, feePips: 3000 }),
    ).toBe(997_000n);
    expect(
      spotQuote({ amountIn: 1_000_000n, sqrtPriceX96: Q96, zeroForOne: false, feePips: 3000 }),
    ).toBe(997_000n);
  });

  it("is direction-sensitive away from price 1", () => {
    const sqrtP = ratio(6000); // price ~1.822
    const outZeroForOne = spotQuote({
      amountIn: 1_000_000n,
      sqrtPriceX96: sqrtP,
      zeroForOne: true,
      feePips: 0,
    });
    const outOneForZero = spotQuote({
      amountIn: 1_000_000n,
      sqrtPriceX96: sqrtP,
      zeroForOne: false,
      feePips: 0,
    });
    // zeroForOne multiplies by the price (>1); oneForZero divides.
    expect(Number(outZeroForOne)).toBeCloseTo(1_000_000 * 1.0001 ** 6000, -1);
    expect(Number(outOneForZero)).toBeCloseTo(1_000_000 / 1.0001 ** 6000, -1);
  });

  it("returns 0 for a zero amount", () => {
    expect(spotQuote({ amountIn: 0n, sqrtPriceX96: Q96, zeroForOne: true, feePips: 3000 })).toBe(0n);
  });

  it("rejects invalid inputs", () => {
    expect(() =>
      spotQuote({ amountIn: -1n, sqrtPriceX96: Q96, zeroForOne: true, feePips: 0 }),
    ).toThrow(/non-negative/);
    expect(() =>
      spotQuote({ amountIn: 1n, sqrtPriceX96: 0n, zeroForOne: true, feePips: 0 }),
    ).toThrow(/positive/);
    expect(() =>
      spotQuote({ amountIn: 1n, sqrtPriceX96: Q96, zeroForOne: true, feePips: 1_000_000 }),
    ).toThrow(/feePips/);
  });
});

describe("minOutFromSlippage", () => {
  it("applies basis points with floor division", () => {
    expect(minOutFromSlippage(1_000_000n, 0)).toBe(1_000_000n);
    expect(minOutFromSlippage(1_000_000n, 50)).toBe(995_000n); // 0.5%
    expect(minOutFromSlippage(1_000_000n, 100)).toBe(990_000n); // 1%
    expect(minOutFromSlippage(1_000_000n, 10_000)).toBe(0n); // 100%
    expect(minOutFromSlippage(3n, 1)).toBe(2n); // floor: 3 * 9999 / 10000
  });

  it("rejects out-of-range slippage", () => {
    expect(() => minOutFromSlippage(1n, -1)).toThrow(/0\.\.10000/);
    expect(() => minOutFromSlippage(1n, 10_001)).toThrow(/0\.\.10000/);
    expect(() => minOutFromSlippage(1n, 0.5)).toThrow(/0\.\.10000/);
  });
});

describe("deadlineHeight", () => {
  it("adds the delta to the tip", () => {
    expect(deadlineHeight(100, 50)).toBe(150);
  });

  it("rejects non-positive deltas", () => {
    expect(() => deadlineHeight(100, 0)).toThrow(/positive/);
    expect(() => deadlineHeight(100, -5)).toThrow(/positive/);
  });
});

describe("token amount formatting", () => {
  it("formats raw amounts", () => {
    expect(formatTokenAmount(0n, 6)).toBe("0");
    expect(formatTokenAmount(1_000_000n, 6)).toBe("1");
    expect(formatTokenAmount(1_500_000n, 6)).toBe("1.5");
    expect(formatTokenAmount(123n, 6)).toBe("0.000123");
  });

  it("parses decimal strings exactly", () => {
    expect(parseTokenAmount("1", 6)).toBe(1_000_000n);
    expect(parseTokenAmount("1.5", 6)).toBe(1_500_000n);
    expect(parseTokenAmount("0.000001", 6)).toBe(1n);
    expect(parseTokenAmount(" 2.25 ", 6)).toBe(2_250_000n);
  });

  it("round-trips", () => {
    for (const raw of [0n, 1n, 999_999n, 1_000_000n, 123_456_789_012n]) {
      expect(parseTokenAmount(formatTokenAmount(raw, 6), 6)).toBe(raw);
    }
  });

  it("rejects invalid amounts", () => {
    expect(() => parseTokenAmount("", 6)).toThrow(/invalid amount/);
    expect(() => parseTokenAmount("-1", 6)).toThrow(/invalid amount/);
    expect(() => parseTokenAmount("1.2.3", 6)).toThrow(/invalid amount/);
    expect(() => parseTokenAmount("abc", 6)).toThrow(/invalid amount/);
    expect(() => parseTokenAmount("1.1234567", 6)).toThrow(/decimal places/);
  });
});
