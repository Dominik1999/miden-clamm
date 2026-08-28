import { describe, it, expect } from "vitest";
import {
  FIELT_MODULUS,
  felt,
  accountIdParts,
  accountIdBytes,
  accountTargetTag,
  u128ToLimbs,
  limbsToU128,
} from "@/lib/clamm/felts";
import { loadGolden } from "./golden";

const golden = loadGolden();

describe("felt", () => {
  it("accepts canonical field elements", () => {
    expect(felt(0n)).toBe(0n);
    expect(felt(FIELT_MODULUS - 1n)).toBe(FIELT_MODULUS - 1n);
  });

  it("rejects out-of-field values", () => {
    expect(() => felt(FIELT_MODULUS)).toThrow(/not a canonical field element/);
    expect(() => felt(-1n)).toThrow(/not a canonical field element/);
  });
});

describe("accountIdParts", () => {
  it.each(Object.entries(golden.accounts))(
    "matches the Rust prefix/suffix felts for the golden %s account",
    (_label, account) => {
      const { prefix, suffix } = accountIdParts(account.hex);
      expect(prefix).toBe(BigInt(account.prefixFelt));
      expect(suffix).toBe(BigInt(account.suffixFelt));
    },
  );

  it("rejects malformed hex", () => {
    expect(() => accountIdParts("0x1234")).toThrow(/invalid account id hex/);
    expect(() => accountIdParts("not-hex")).toThrow(/invalid account id hex/);
    // 32 hex chars (16 bytes) is a Word, not an account id.
    expect(() => accountIdParts(`0x${"ab".repeat(16)}`)).toThrow(/invalid account id hex/);
  });

  it("round-trips through accountIdBytes", () => {
    const hex = golden.accounts.user.hex;
    const bytes = accountIdBytes(hex);
    expect(bytes).toHaveLength(15);
    const rebuilt =
      "0x" + Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    expect(rebuilt).toBe(hex.toLowerCase());
  });
});

describe("accountTargetTag", () => {
  it("matches the golden NoteTag::with_account_target(pool) value", () => {
    const swap = golden.notes.find((n) => n.kind === "swap")!;
    // Network notes are tagged with the POOL target (ntx-builder tag
    // routing), not the sender-derived default.
    expect(accountTargetTag(golden.accounts.pool.hex)).toBe(swap.tag);
    expect(accountTargetTag(swap.senderHex)).not.toBe(swap.tag);
  });

  it("keeps only the top 14 bits of the prefix's high u32", () => {
    // prefix high u32 = 0x1236dc78; top 14 bits mask 0xFFFC0000 -> 0x12340000
    const hex = "0x1236dc780000000000000000000000";
    expect(accountTargetTag(hex)).toBe(0x12340000);
  });
});

describe("u128 limb packing", () => {
  it("splits into little-endian u32 limbs (Rust u128_limb_felts mirror)", () => {
    expect(u128ToLimbs(0n)).toEqual([0n, 0n, 0n, 0n]);
    expect(u128ToLimbs(1_000_000_000_000n)).toEqual([
      1_000_000_000_000n & 0xffffffffn,
      1_000_000_000_000n >> 32n,
      0n,
      0n,
    ]);
    const max = 2n ** 128n - 1n;
    expect(u128ToLimbs(max)).toEqual([
      0xffffffffn,
      0xffffffffn,
      0xffffffffn,
      0xffffffffn,
    ]);
  });

  it("round-trips through limbsToU128", () => {
    for (const value of [0n, 1n, 0xffffffffn, 2n ** 96n + 12345n, 2n ** 128n - 1n]) {
      expect(limbsToU128(u128ToLimbs(value))).toBe(value);
    }
  });

  it("rejects out-of-range inputs", () => {
    expect(() => u128ToLimbs(-1n)).toThrow(/u128/);
    expect(() => u128ToLimbs(2n ** 128n)).toThrow(/u128/);
    expect(() => limbsToU128([0n, 0n, 0n])).toThrow(/expected 4 limbs/);
    expect(() => limbsToU128([2n ** 32n, 0n, 0n, 0n])).toThrow(/exceeds u32/);
  });
});
