import { useMemo, useState } from "react";
import type { ClammDeployment } from "@/lib/clamm/deployment";
import type { PoolState } from "@/hooks/clamm/usePoolState";
import type { SwapParams, SubmitStage } from "@/hooks/clamm/useSubmitPoolNote";
import {
  spotQuote,
  minOutFromSlippage,
  deadlineHeight,
  parseTokenAmount,
  formatTokenAmount,
} from "@/lib/clamm/price";

export interface SwapFormProps {
  deployment: ClammDeployment;
  poolState: PoolState | null;
  balances: { token0: bigint; token1: bigint };
  onSubmit: (params: SwapParams) => Promise<unknown>;
  stage: SubmitStage;
  error: string | null;
  disabled?: boolean;
}

const DEFAULT_SLIPPAGE_PCT = "0.5";
const DEFAULT_DEADLINE_BLOCKS = "100";

/**
 * Swap form: direction, amount in, slippage tolerance -> min_amount_out,
 * deadline in blocks from the current tip. Builds the swap network note.
 */
export function SwapForm({
  deployment,
  poolState,
  balances,
  onSubmit,
  stage,
  error,
  disabled,
}: SwapFormProps) {
  const [direction, setDirection] = useState<0 | 1>(0);
  const [amountText, setAmountText] = useState("");
  const [slippageText, setSlippageText] = useState(DEFAULT_SLIPPAGE_PCT);
  const [deadlineText, setDeadlineText] = useState(DEFAULT_DEADLINE_BLOCKS);
  const [formError, setFormError] = useState<string | null>(null);

  const tokenIn = direction === 0 ? deployment.token0 : deployment.token1;
  const tokenOut = direction === 0 ? deployment.token1 : deployment.token0;
  const balanceIn = direction === 0 ? balances.token0 : balances.token1;

  const parsed = useMemo(() => {
    if (!amountText.trim() || !poolState) return null;
    try {
      const amountIn = parseTokenAmount(amountText, tokenIn.decimals);
      const slippagePct = Number(slippageText);
      if (!Number.isFinite(slippagePct) || slippagePct < 0 || slippagePct > 100) {
        throw new Error("Slippage must be between 0 and 100%");
      }
      const slippageBps = Math.round(slippagePct * 100);
      const quote = spotQuote({
        amountIn,
        sqrtPriceX96: poolState.sqrtPriceX96,
        zeroForOne: direction === 0,
        feePips: poolState.feePips,
      });
      const minOut = minOutFromSlippage(quote, slippageBps);
      const deadlineBlocks = Number(deadlineText);
      if (!Number.isInteger(deadlineBlocks) || deadlineBlocks <= 0) {
        throw new Error("Deadline must be a positive number of blocks");
      }
      return { amountIn, quote, minOut, deadlineBlocks, error: null as string | null };
    } catch (err) {
      return {
        amountIn: 0n,
        quote: 0n,
        minOut: 0n,
        deadlineBlocks: 0,
        error: err instanceof Error ? err.message : String(err),
      };
    }
  }, [amountText, slippageText, deadlineText, direction, poolState, tokenIn.decimals]);

  const insufficient = parsed !== null && !parsed.error && parsed.amountIn > balanceIn;

  const canSubmit =
    !disabled &&
    stage !== "building" &&
    stage !== "submitting" &&
    poolState !== null &&
    parsed !== null &&
    !parsed.error &&
    parsed.amountIn > 0n &&
    !insufficient;

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!canSubmit || !parsed || !poolState) return;
    setFormError(null);
    try {
      await onSubmit({
        direction,
        amountIn: parsed.amountIn,
        minOut: parsed.minOut,
        deadline: deadlineHeight(poolState.blockHeight, parsed.deadlineBlocks),
      });
      setAmountText("");
    } catch (err) {
      setFormError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <form className="clamm-card" onSubmit={handleSubmit} aria-label="Swap">
      <h3>Swap</h3>
      <div className="clamm-field">
        <label htmlFor="swap-direction">Direction</label>
        <select
          id="swap-direction"
          value={direction}
          onChange={(e) => setDirection(Number(e.target.value) as 0 | 1)}
        >
          <option value={0}>
            {deployment.token0.symbol} → {deployment.token1.symbol}
          </option>
          <option value={1}>
            {deployment.token1.symbol} → {deployment.token0.symbol}
          </option>
        </select>
      </div>
      <div className="clamm-field">
        <label htmlFor="swap-amount">Amount in ({tokenIn.symbol})</label>
        <input
          id="swap-amount"
          value={amountText}
          onChange={(e) => setAmountText(e.target.value)}
          placeholder="0.0"
          inputMode="decimal"
        />
        <span className="clamm-hint">
          Balance: {formatTokenAmount(balanceIn, tokenIn.decimals)} {tokenIn.symbol}
        </span>
      </div>
      <div className="clamm-field">
        <label htmlFor="swap-slippage">Slippage tolerance (%)</label>
        <input
          id="swap-slippage"
          value={slippageText}
          onChange={(e) => setSlippageText(e.target.value)}
          inputMode="decimal"
        />
      </div>
      <div className="clamm-field">
        <label htmlFor="swap-deadline">Deadline (blocks from now)</label>
        <input
          id="swap-deadline"
          value={deadlineText}
          onChange={(e) => setDeadlineText(e.target.value)}
          inputMode="numeric"
        />
      </div>

      {parsed && !parsed.error && parsed.amountIn > 0n && (
        <p className="clamm-quote" data-testid="swap-quote">
          Expected out (spot): {formatTokenAmount(parsed.quote, tokenOut.decimals)}{" "}
          {tokenOut.symbol} · Min out: {formatTokenAmount(parsed.minOut, tokenOut.decimals)}{" "}
          {tokenOut.symbol}
        </p>
      )}
      {parsed?.error && amountText.trim() !== "" && (
        <p className="error" role="alert">
          {parsed.error}
        </p>
      )}
      {insufficient && (
        <p className="error" role="alert">
          Insufficient {tokenIn.symbol} balance
        </p>
      )}
      {(error || formError) && (
        <p className="error" role="alert">
          {error ?? formError}
        </p>
      )}

      <button type="submit" disabled={!canSubmit}>
        {stage === "building"
          ? "Building note..."
          : stage === "submitting"
            ? "Submitting..."
            : "Submit swap"}
      </button>
    </form>
  );
}
