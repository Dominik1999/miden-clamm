// Pure-TS field-element and account-id helpers for the CLAMM frontend.
//
// Every encoding here mirrors the Rust integration testbed
// (project-template/integration/src/pool.rs) exactly and is cross-checked by
// unit tests against golden vectors exported from the Rust side
// (public/packages/clamm/golden.json, written by export_web_artifacts).

/** The Miden field modulus: 2^64 - 2^32 + 1. */
export const FIELT_MODULUS = 2n ** 64n - 2n ** 32n + 1n;

/** Asserts a value is a canonical field element and returns it. */
export function felt(value: bigint): bigint {
  if (value < 0n || value >= FIELT_MODULUS) {
    throw new Error(`value is not a canonical field element: ${value}`);
  }
  return value;
}

/** Parses a 15-byte account-id hex string (0x + 30 hex chars). */
function accountIdBytesFromHex(hex: string): Uint8Array {
  if (!/^0x[0-9a-fA-F]{30}$/.test(hex)) {
    throw new Error(`invalid account id hex (expected 0x + 30 hex chars): ${hex}`);
  }
  const bytes = new Uint8Array(15);
  for (let i = 0; i < 15; i++) {
    bytes[i] = parseInt(hex.slice(2 + 2 * i, 4 + 2 * i), 16);
  }
  return bytes;
}

/**
 * Splits an account id (hex form) into its `(prefix, suffix)` field elements,
 * mirroring the Rust `AccountId::prefix()` / `AccountId::suffix()` values that
 * the note storage layouts and the NetworkAccountTarget attachment carry.
 *
 * Byte layout (miden-protocol `AccountIdV1 -> [u8; 15]`): the first 8 bytes
 * are the prefix u64 big-endian; the last 7 bytes are the suffix's high 7
 * bytes big-endian (the suffix's lowest byte is always zero).
 */
export function accountIdParts(hex: string): { prefix: bigint; suffix: bigint } {
  const bytes = accountIdBytesFromHex(hex);
  let prefix = 0n;
  for (let i = 0; i < 8; i++) prefix = (prefix << 8n) | BigInt(bytes[i]);
  let suffix = 0n;
  for (let i = 8; i < 15; i++) suffix = (suffix << 8n) | BigInt(bytes[i]);
  suffix <<= 8n; // the dropped lowest byte is always zero
  return { prefix: felt(prefix), suffix: felt(suffix) };
}

/** Returns the 15 raw bytes of an account id hex string. */
export function accountIdBytes(hex: string): Uint8Array {
  return accountIdBytesFromHex(hex);
}

/**
 * Computes `NoteTag::with_account_target(accountId).as_u32()`: the 14 most
 * significant bits of the account-id prefix's high u32, with the rest zeroed
 * (miden-protocol `NoteTag::with_custom_account_target`,
 * `DEFAULT_ACCOUNT_TARGET_TAG_LENGTH = 14` => mask 0xFFFC0000).
 */
export function accountTargetTag(hex: string): number {
  const { prefix } = accountIdParts(hex);
  const high32 = Number((prefix >> 32n) & 0xffffffffn);
  return (high32 & 0xfffc0000) >>> 0;
}

/** Splits a u128 into the 4 little-endian u32-limb felts used by the pool. */
export function u128ToLimbs(x: bigint): [bigint, bigint, bigint, bigint] {
  if (x < 0n || x >= 2n ** 128n) {
    throw new Error(`value out of u128 range: ${x}`);
  }
  const mask = 0xffffffffn;
  return [x & mask, (x >> 32n) & mask, (x >> 64n) & mask, (x >> 96n) & mask];
}

/** Recombines 4 little-endian u32-limb felts into a u128. */
export function limbsToU128(limbs: readonly bigint[]): bigint {
  if (limbs.length !== 4) {
    throw new Error(`expected 4 limbs, got ${limbs.length}`);
  }
  let x = 0n;
  for (let i = 0; i < 4; i++) {
    const limb = limbs[i];
    if (limb < 0n || limb > 0xffffffffn) {
      throw new Error(`storage limb exceeds u32: ${limb}`);
    }
    x |= limb << BigInt(32 * i);
  }
  return x;
}
