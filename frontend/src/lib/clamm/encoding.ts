// Note-storage felt layouts for the four CLAMM pool notes, mirroring
// project-template/integration/src/bin/validate_local_masm.rs
// (`swap_storage` / `mint_storage` and the inline burn/collect layouts) and
// DESIGN.md Part 2 exactly. Cross-checked against golden vectors from the
// Rust exporter.

import { accountIdParts, u128ToLimbs, felt } from "./felts";

/** Tick offset encoding: stored tick = tick + 2^19. */
export const TICK_OFFSET = 1 << 19;

/** NetworkAccountTarget attachment scheme (miden-standards scheme 2). */
export const NETWORK_ACCOUNT_TARGET_SCHEME = 2;

/** `NoteExecutionHint::Always` encoded as a felt (tag 1, payload 0). */
export const EXECUTION_HINT_ALWAYS = 1n;

/** Encodes a tick as its offset-encoded storage felt. */
export function tickToFelt(tick: number): bigint {
  if (!Number.isInteger(tick)) throw new Error(`tick must be an integer: ${tick}`);
  const stored = tick + TICK_OFFSET;
  if (stored < 0 || stored > 0xffffffff) {
    throw new Error(`tick out of encodable range: ${tick}`);
  }
  return BigInt(stored);
}

/** Decodes an offset-encoded storage felt back to a tick. */
export function feltToTick(value: bigint): number {
  return Number(value) - TICK_OFFSET;
}

/**
 * Swap-note storage:
 * `[pool_suffix, pool_prefix, direction, min_out_lo, min_out_hi,
 *   recipient_suffix, recipient_prefix, deadline_height]`.
 */
export function swapStorage(params: {
  poolHex: string;
  direction: 0 | 1; // 0 = zero_for_one (token0 in), 1 = one_for_zero (token1 in)
  minOut: bigint;
  recipientHex: string;
  deadline: number;
}): bigint[] {
  if (params.minOut < 0n || params.minOut >= 2n ** 64n) {
    throw new Error(`min_out out of u64 range: ${params.minOut}`);
  }
  const pool = accountIdParts(params.poolHex);
  const recipient = accountIdParts(params.recipientHex);
  return [
    pool.suffix,
    pool.prefix,
    BigInt(params.direction),
    params.minOut & 0xffffffffn,
    params.minOut >> 32n,
    recipient.suffix,
    recipient.prefix,
    felt(BigInt(params.deadline)),
  ];
}

/**
 * Mint-note storage:
 * `[pool_suffix, pool_prefix, tickLower, tickUpper, liq_limb0..3, deadline]`.
 */
export function mintStorage(params: {
  poolHex: string;
  tickLower: number;
  tickUpper: number;
  liquidity: bigint;
  deadline: number;
}): bigint[] {
  const pool = accountIdParts(params.poolHex);
  const limbs = u128ToLimbs(params.liquidity);
  return [
    pool.suffix,
    pool.prefix,
    tickToFelt(params.tickLower),
    tickToFelt(params.tickUpper),
    limbs[0],
    limbs[1],
    limbs[2],
    limbs[3],
    felt(BigInt(params.deadline)),
  ];
}

/**
 * Burn-note storage:
 * `[pool_suffix, pool_prefix, tickLower, tickUpper, liq_limb0..3]`.
 */
export function burnStorage(params: {
  poolHex: string;
  tickLower: number;
  tickUpper: number;
  liquidity: bigint;
}): bigint[] {
  const pool = accountIdParts(params.poolHex);
  const limbs = u128ToLimbs(params.liquidity);
  return [
    pool.suffix,
    pool.prefix,
    tickToFelt(params.tickLower),
    tickToFelt(params.tickUpper),
    limbs[0],
    limbs[1],
    limbs[2],
    limbs[3],
  ];
}

/** Collect-note storage: `[pool_suffix, pool_prefix, tickLower, tickUpper]`. */
export function collectStorage(params: {
  poolHex: string;
  tickLower: number;
  tickUpper: number;
}): bigint[] {
  const pool = accountIdParts(params.poolHex);
  return [pool.suffix, pool.prefix, tickToFelt(params.tickLower), tickToFelt(params.tickUpper)];
}

/**
 * The single-word `NetworkAccountTarget` attachment content targeting the
 * pool: `[pool_suffix, pool_prefix, exec_hint(always) = 1, 0]`
 * (miden-standards `NetworkAccountTarget -> NoteAttachment`).
 */
export function networkTargetWord(poolHex: string): [bigint, bigint, bigint, bigint] {
  const pool = accountIdParts(poolHex);
  return [pool.suffix, pool.prefix, EXECUTION_HINT_ALWAYS, 0n];
}

/** P2ID note storage: `[target_suffix, target_prefix]`. */
export function p2idStorage(targetHex: string): bigint[] {
  const target = accountIdParts(targetHex);
  return [target.suffix, target.prefix];
}

/** Guest P2ID serial-derivation salts (validate_local_masm constants). */
export const P2ID_SALT = {
  swapOut: 0,
  swapRefund: 1,
  mintRefund: 2,
  collect: 3,
} as const;
