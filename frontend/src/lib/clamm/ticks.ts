// Tick-range validation for mint/burn/collect forms.
//
// The supported tick range is ±443,636 (DESIGN.md Part 3: sqrtPriceX96 in
// u128 restricts the range to half of Uniswap's ±887,272). Position bounds
// must be aligned to the pool's tick spacing.

export const MIN_TICK = -443_636;
export const MAX_TICK = 443_636;

/** Returns true if the tick is aligned to the spacing (sign-safe). */
export function isTickAligned(tick: number, spacing: number): boolean {
  if (!Number.isInteger(tick) || !Number.isInteger(spacing) || spacing <= 0) return false;
  return ((tick % spacing) + spacing) % spacing === 0;
}

/** Rounds a tick down to the nearest spacing-aligned tick. */
export function alignTickDown(tick: number, spacing: number): number {
  if (!Number.isInteger(spacing) || spacing <= 0) throw new Error(`invalid spacing: ${spacing}`);
  return Math.floor(tick / spacing) * spacing;
}

/**
 * Validates a position's tick range. Returns `null` when valid, otherwise a
 * human-readable error message.
 */
export function validateTickRange(
  lower: number,
  upper: number,
  spacing: number,
): string | null {
  if (!Number.isInteger(lower) || !Number.isInteger(upper)) {
    return "Ticks must be integers";
  }
  if (!Number.isInteger(spacing) || spacing <= 0) {
    return "Invalid tick spacing";
  }
  if (lower < MIN_TICK || upper > MAX_TICK) {
    return `Ticks must be within ±${MAX_TICK.toLocaleString("en-US")}`;
  }
  if (lower >= upper) {
    return "Lower tick must be below upper tick";
  }
  if (!isTickAligned(lower, spacing) || !isTickAligned(upper, spacing)) {
    return `Ticks must be multiples of the pool tick spacing (${spacing})`;
  }
  return null;
}
