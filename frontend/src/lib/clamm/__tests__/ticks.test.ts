import { describe, it, expect } from "vitest";
import {
  MIN_TICK,
  MAX_TICK,
  isTickAligned,
  alignTickDown,
  validateTickRange,
} from "@/lib/clamm/ticks";

describe("tick range constants", () => {
  it("uses the DESIGN Part 3 supported range", () => {
    expect(MIN_TICK).toBe(-443_636);
    expect(MAX_TICK).toBe(443_636);
  });
});

describe("isTickAligned", () => {
  it("handles negative ticks (sign-safe modulo)", () => {
    expect(isTickAligned(-120, 60)).toBe(true);
    expect(isTickAligned(-121, 60)).toBe(false);
    expect(isTickAligned(120, 60)).toBe(true);
    expect(isTickAligned(0, 60)).toBe(true);
    expect(isTickAligned(59, 60)).toBe(false);
  });

  it("rejects invalid inputs", () => {
    expect(isTickAligned(1.5, 60)).toBe(false);
    expect(isTickAligned(60, 0)).toBe(false);
    expect(isTickAligned(60, -60)).toBe(false);
  });
});

describe("alignTickDown", () => {
  it("rounds toward negative infinity", () => {
    expect(alignTickDown(125, 60)).toBe(120);
    expect(alignTickDown(-125, 60)).toBe(-180);
    expect(alignTickDown(-120, 60)).toBe(-120);
    expect(alignTickDown(0, 60)).toBe(0);
  });

  it("rejects invalid spacing", () => {
    expect(() => alignTickDown(0, 0)).toThrow(/invalid spacing/);
  });
});

describe("validateTickRange", () => {
  it("accepts a valid aligned range", () => {
    expect(validateTickRange(-120, 120, 60)).toBeNull();
    expect(validateTickRange(MIN_TICK + 1, MAX_TICK - 1, 60)).not.toBeNull(); // unaligned bounds
    expect(validateTickRange(-443_580, 443_580, 60)).toBeNull(); // aligned, in range
  });

  it("rejects non-integer ticks", () => {
    expect(validateTickRange(1.5, 120, 60)).toMatch(/integers/);
    expect(validateTickRange(-120, Number.NaN, 60)).toMatch(/integers/);
  });

  it("rejects out-of-range ticks (±443,636)", () => {
    expect(validateTickRange(MIN_TICK - 60, 0, 60)).toMatch(/443,636/);
    expect(validateTickRange(0, MAX_TICK + 60, 60)).toMatch(/443,636/);
  });

  it("rejects inverted or empty ranges", () => {
    expect(validateTickRange(120, -120, 60)).toMatch(/below/);
    expect(validateTickRange(120, 120, 60)).toMatch(/below/);
  });

  it("rejects misaligned ticks", () => {
    expect(validateTickRange(-121, 120, 60)).toMatch(/multiples/);
    expect(validateTickRange(-120, 121, 60)).toMatch(/multiples/);
  });

  it("rejects invalid spacing", () => {
    expect(validateTickRange(-120, 120, 0)).toMatch(/spacing/i);
  });
});
