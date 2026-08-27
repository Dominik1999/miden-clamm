import { useState } from "react";
import type { ClammDeployment } from "@/lib/clamm/deployment";
import type { PoolState } from "@/hooks/clamm/usePoolState";
import type {
  MintParams,
  BurnParams,
  CollectParams,
  SubmitStage,
} from "@/hooks/clamm/useSubmitPoolNote";
import type { TrackedPosition } from "@/lib/clamm/store";
import { validateTickRange } from "@/lib/clamm/ticks";
import { deadlineHeight, parseTokenAmount } from "@/lib/clamm/price";

export interface PositionsProps {
  deployment: ClammDeployment;
  poolState: PoolState | null;
  positions: TrackedPosition[];
  onMint: (params: MintParams) => Promise<unknown>;
  onBurn: (params: BurnParams) => Promise<unknown>;
  onCollect: (params: CollectParams) => Promise<unknown>;
  stage: SubmitStage;
  error: string | null;
  disabled?: boolean;
}

const DEFAULT_DEADLINE_BLOCKS = 100;

/** Mint form + position list with burn/collect actions. */
export function Positions({
  deployment,
  poolState,
  positions,
  onMint,
  onBurn,
  onCollect,
  stage,
  error,
  disabled,
}: PositionsProps) {
  const spacing = poolState?.tickSpacing ?? deployment.pool.tickSpacing;
  const [lowerText, setLowerText] = useState("-120");
  const [upperText, setUpperText] = useState("120");
  const [liquidityText, setLiquidityText] = useState("");
  const [amount0Text, setAmount0Text] = useState("");
  const [amount1Text, setAmount1Text] = useState("");
  const [formError, setFormError] = useState<string | null>(null);

  const busy = stage === "building" || stage === "submitting";

  const handleMint = async (event: React.FormEvent) => {
    event.preventDefault();
    setFormError(null);
    try {
      if (!poolState) throw new Error("Pool state not loaded yet");
      const lower = Number(lowerText);
      const upper = Number(upperText);
      const rangeError = validateTickRange(lower, upper, spacing);
      if (rangeError) throw new Error(rangeError);
      if (!/^\d+$/.test(liquidityText.trim())) {
        throw new Error("Liquidity must be a positive integer");
      }
      const liquidity = BigInt(liquidityText.trim());
      if (liquidity <= 0n) throw new Error("Liquidity must be a positive integer");
      const amount0Max = amount0Text.trim()
        ? parseTokenAmount(amount0Text, deployment.token0.decimals)
        : 0n;
      const amount1Max = amount1Text.trim()
        ? parseTokenAmount(amount1Text, deployment.token1.decimals)
        : 0n;
      if (amount0Max === 0n && amount1Max === 0n) {
        throw new Error("Provide at least one max deposit amount");
      }
      await onMint({
        tickLower: lower,
        tickUpper: upper,
        liquidity,
        amount0Max,
        amount1Max,
        deadline: deadlineHeight(poolState.blockHeight, DEFAULT_DEADLINE_BLOCKS),
      });
    } catch (err) {
      setFormError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <div>
      <form className="clamm-card" onSubmit={handleMint} aria-label="Mint position">
        <h3>Add liquidity</h3>
        <div className="clamm-field-row">
          <div className="clamm-field">
            <label htmlFor="mint-lower">Tick lower</label>
            <input
              id="mint-lower"
              value={lowerText}
              onChange={(e) => setLowerText(e.target.value)}
              inputMode="numeric"
            />
          </div>
          <div className="clamm-field">
            <label htmlFor="mint-upper">Tick upper</label>
            <input
              id="mint-upper"
              value={upperText}
              onChange={(e) => setUpperText(e.target.value)}
              inputMode="numeric"
            />
          </div>
        </div>
        <p className="clamm-hint">
          Ticks must be multiples of {spacing}, within ±443,636.
        </p>
        <div className="clamm-field">
          <label htmlFor="mint-liquidity">Liquidity (raw)</label>
          <input
            id="mint-liquidity"
            value={liquidityText}
            onChange={(e) => setLiquidityText(e.target.value)}
            placeholder="1000000000000"
            inputMode="numeric"
          />
        </div>
        <div className="clamm-field-row">
          <div className="clamm-field">
            <label htmlFor="mint-amount0">Max {deployment.token0.symbol} deposit</label>
            <input
              id="mint-amount0"
              value={amount0Text}
              onChange={(e) => setAmount0Text(e.target.value)}
              placeholder="0.0"
              inputMode="decimal"
            />
          </div>
          <div className="clamm-field">
            <label htmlFor="mint-amount1">Max {deployment.token1.symbol} deposit</label>
            <input
              id="mint-amount1"
              value={amount1Text}
              onChange={(e) => setAmount1Text(e.target.value)}
              placeholder="0.0"
              inputMode="decimal"
            />
          </div>
        </div>
        <p className="clamm-hint">
          Unused deposit comes back as a refund note (claim it in Activity).
        </p>
        {formError && (
          <p className="error" role="alert">
            {formError}
          </p>
        )}
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <button type="submit" disabled={disabled || busy || !poolState}>
          {busy ? "Working..." : "Mint position"}
        </button>
      </form>

      <div className="clamm-card">
        <h3>Your positions</h3>
        {positions.length === 0 ? (
          <p>No positions minted from this browser yet.</p>
        ) : (
          <table className="clamm-table">
            <thead>
              <tr>
                <th>Range</th>
                <th>Liquidity</th>
                <th>Actions</th>
              </tr>
            </thead>
            <tbody>
              {positions.map((position) => (
                <tr key={`${position.tickLower}:${position.tickUpper}`}>
                  <td>
                    [{position.tickLower}, {position.tickUpper}]
                  </td>
                  <td>{position.liquidity}</td>
                  <td>
                    <button
                      type="button"
                      disabled={disabled || busy || position.liquidity === "0"}
                      onClick={() =>
                        void onBurn({
                          tickLower: position.tickLower,
                          tickUpper: position.tickUpper,
                          liquidity: BigInt(position.liquidity),
                        })
                      }
                    >
                      Burn
                    </button>
                    <button
                      type="button"
                      disabled={disabled || busy}
                      onClick={() =>
                        void onCollect({
                          tickLower: position.tickLower,
                          tickUpper: position.tickUpper,
                        })
                      }
                    >
                      Collect
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <p className="clamm-hint">
          Positions are derived from notes submitted in this browser. Burn moves
          principal + fees into the position&apos;s owed balance; Collect pays it
          out as a P2ID note.
        </p>
      </div>
    </div>
  );
}
