import type { PoolState } from "@/hooks/clamm/usePoolState";
import type { ClammTokenInfo } from "@/lib/clamm/deployment";
import { sqrtPriceX96ToPrice, formatPrice } from "@/lib/clamm/price";

export interface PoolViewProps {
  poolState: PoolState | null;
  isLoading: boolean;
  error: string | null;
  token0: ClammTokenInfo;
  token1: ClammTokenInfo;
}

/** Read-only view of the pool's current on-chain state. */
export function PoolView({ poolState, isLoading, error, token0, token1 }: PoolViewProps) {
  if (error) {
    return (
      <div className="clamm-card">
        <p className="error" role="alert">
          Failed to read pool state: {error}
        </p>
      </div>
    );
  }
  if (isLoading || !poolState) {
    return (
      <div className="clamm-card">
        <p>Loading pool state...</p>
      </div>
    );
  }

  const price = sqrtPriceX96ToPrice(
    poolState.sqrtPriceX96,
    token0.decimals,
    token1.decimals,
  );

  return (
    <div className="clamm-card">
      <h3>
        {token0.symbol} / {token1.symbol} pool
      </h3>
      <dl className="clamm-stats">
        <div>
          <dt>Price</dt>
          <dd data-testid="pool-price">
            {formatPrice(price)} {token1.symbol} per {token0.symbol}
          </dd>
        </div>
        <div>
          <dt>Current tick</dt>
          <dd data-testid="pool-tick">{poolState.tick}</dd>
        </div>
        <div>
          <dt>Active liquidity</dt>
          <dd data-testid="pool-liquidity">{poolState.liquidity.toString()}</dd>
        </div>
        <div>
          <dt>Fee tier</dt>
          <dd data-testid="pool-fee">{(poolState.feePips / 10_000).toFixed(2)}%</dd>
        </div>
        <div>
          <dt>Tick spacing</dt>
          <dd>{poolState.tickSpacing}</dd>
        </div>
        <div>
          <dt>Block height</dt>
          <dd data-testid="pool-block">{poolState.blockHeight}</dd>
        </div>
      </dl>
      <p className="clamm-hint">
        sqrtPriceX96: <code>{poolState.sqrtPriceX96.toString()}</code>
      </p>
    </div>
  );
}
