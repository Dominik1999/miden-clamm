import type { ClammDeployment } from "@/lib/clamm/deployment";
import { formatTokenAmount } from "@/lib/clamm/price";

export interface WalletPanelProps {
  deployment: ClammDeployment;
  walletId: string | null;
  balances: { token0: bigint; token1: bigint };
  status: string | null;
  error: string | null;
  onFund: (token: "token0" | "token1") => Promise<unknown>;
  onRefresh: () => Promise<unknown>;
  isBusy: boolean;
}

function shortId(id: string): string {
  return id.length > 14 ? `${id.slice(0, 10)}…${id.slice(-4)}` : id;
}

/** Session wallet: id, balances, and dev-faucet funding actions. */
export function WalletPanel({
  deployment,
  walletId,
  balances,
  status,
  error,
  onFund,
  onRefresh,
  isBusy,
}: WalletPanelProps) {
  return (
    <div className="clamm-card clamm-wallet">
      <h3>Session wallet</h3>
      {walletId ? (
        <>
          <p>
            <code title={walletId}>{shortId(walletId)}</code>
          </p>
          <p data-testid="wallet-balances">
            {formatTokenAmount(balances.token0, deployment.token0.decimals)}{" "}
            {deployment.token0.symbol} ·{" "}
            {formatTokenAmount(balances.token1, deployment.token1.decimals)}{" "}
            {deployment.token1.symbol}
          </p>
          <div className="clamm-actions">
            <button type="button" disabled={isBusy} onClick={() => void onFund("token0")}>
              Get {deployment.token0.symbol}
            </button>
            <button type="button" disabled={isBusy} onClick={() => void onFund("token1")}>
              Get {deployment.token1.symbol}
            </button>
            <button type="button" disabled={isBusy} onClick={() => void onRefresh()}>
              Refresh
            </button>
          </div>
        </>
      ) : (
        <p>{status ?? "Setting up session wallet..."}</p>
      )}
      {status && walletId && <p className="clamm-hint">{status}</p>}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <p className="clamm-hint">
        Demo faucets: TKA/TKB are worthless test tokens. Funding mints them
        with demo faucet keys published in the deployment descriptor, signed
        locally in this browser.
      </p>
    </div>
  );
}
