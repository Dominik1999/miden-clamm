// Price and quote math for the CLAMM pool.
//
// The pool stores `sqrtPriceX96`: sqrt(token1/token0 price) scaled by 2^96,
// held in a u128 (DESIGN.md Part 3). All quote math here is exact bigint
// arithmetic; float conversion happens only at the display boundary.

/** 2^96 — the sqrt-price scaling factor. */
export const Q96 = 2n ** 96n;

/** 2^192 — (2^96)^2, the price scaling factor. */
export const Q192 = Q96 * Q96;

/** Fee denominator: fees are expressed in pips (hundredths of a bip). */
export const FEE_DENOMINATOR = 1_000_000n;

/** Slippage denominator: slippage is expressed in basis points. */
export const BPS_DENOMINATOR = 10_000n;

const DISPLAY_SCALE = 10n ** 18n;

/**
 * Converts a sqrtPriceX96 into the human price of token0 in units of token1
 * (token1 per token0), adjusted for token decimals.
 */
export function sqrtPriceX96ToPrice(
  sqrtPriceX96: bigint,
  decimals0: number,
  decimals1: number,
): number {
  if (sqrtPriceX96 <= 0n) return 0;
  let scaled = (sqrtPriceX96 * sqrtPriceX96 * DISPLAY_SCALE) / Q192;
  const decimalDiff = decimals0 - decimals1;
  if (decimalDiff > 0) scaled *= 10n ** BigInt(decimalDiff);
  if (decimalDiff < 0) scaled /= 10n ** BigInt(-decimalDiff);
  return Number(scaled) / 1e18;
}

/** Formats a price for display with adaptive precision. */
export function formatPrice(price: number): string {
  if (!Number.isFinite(price) || price === 0) return "0";
  if (price >= 1000) return price.toFixed(2);
  if (price >= 1) return price.toFixed(4);
  if (price >= 0.0001) return price.toFixed(6);
  return price.toExponential(4);
}

/**
 * Spot-price quote for a swap: expected output at the CURRENT pool price,
 * after the pool fee, ignoring price impact and tick crossings. This is the
 * v1 quoting model for the UI (the pool itself enforces `min_amount_out`
 * exactly); large swaps will receive less than this quote, which is why the
 * slippage tolerance below is applied on top.
 *
 * zeroForOne (direction 0): token0 in, token1 out — out = in' * P
 * oneForZero (direction 1): token1 in, token0 out — out = in' / P
 * where P = sqrtPriceX96^2 / 2^192 and in' = in * (1e6 - feePips) / 1e6.
 */
export function spotQuote(params: {
  amountIn: bigint;
  sqrtPriceX96: bigint;
  zeroForOne: boolean;
  feePips: number;
}): bigint {
  const { amountIn, sqrtPriceX96, zeroForOne, feePips } = params;
  if (amountIn < 0n) throw new Error("amountIn must be non-negative");
  if (sqrtPriceX96 <= 0n) throw new Error("sqrtPriceX96 must be positive");
  if (feePips < 0 || feePips >= 1_000_000) throw new Error(`invalid feePips: ${feePips}`);
  const afterFee = (amountIn * (FEE_DENOMINATOR - BigInt(feePips))) / FEE_DENOMINATOR;
  if (zeroForOne) {
    return (afterFee * sqrtPriceX96 * sqrtPriceX96) / Q192;
  }
  return (afterFee * Q192) / (sqrtPriceX96 * sqrtPriceX96);
}

/**
 * Applies a slippage tolerance (basis points) to a quoted output:
 * `min_out = quote * (10000 - slippageBps) / 10000`, floor.
 */
export function minOutFromSlippage(quote: bigint, slippageBps: number): bigint {
  if (!Number.isInteger(slippageBps) || slippageBps < 0 || slippageBps > 10_000) {
    throw new Error(`slippage must be an integer 0..10000 bps: ${slippageBps}`);
  }
  if (quote < 0n) throw new Error("quote must be non-negative");
  return (quote * (BPS_DENOMINATOR - BigInt(slippageBps))) / BPS_DENOMINATOR;
}

/** Deadline block height from the current tip and a blocks-from-now delta. */
export function deadlineHeight(currentBlock: number, blocksFromNow: number): number {
  if (!Number.isInteger(blocksFromNow) || blocksFromNow <= 0) {
    throw new Error(`deadline delta must be a positive integer: ${blocksFromNow}`);
  }
  return currentBlock + blocksFromNow;
}

/** Formats a raw token amount using the given decimals. */
export function formatTokenAmount(amount: bigint, decimals: number): string {
  const base = 10n ** BigInt(decimals);
  const whole = amount / base;
  const frac = amount % base;
  if (frac === 0n) return whole.toString();
  const fracStr = frac.toString().padStart(decimals, "0").replace(/0+$/, "");
  return `${whole}.${fracStr}`;
}

/** Parses a decimal string into a raw token amount. Throws on invalid input. */
export function parseTokenAmount(text: string, decimals: number): bigint {
  const trimmed = text.trim();
  if (!/^\d+(\.\d+)?$/.test(trimmed)) {
    throw new Error(`invalid amount: "${text}"`);
  }
  const [whole, frac = ""] = trimmed.split(".");
  if (frac.length > decimals) {
    throw new Error(`too many decimal places (max ${decimals}): "${text}"`);
  }
  const fracPadded = frac.padEnd(decimals, "0");
  return BigInt(whole) * 10n ** BigInt(decimals) + BigInt(fracPadded || "0");
}
