import type { ActivityItem } from "@/hooks/clamm/useNoteLifecycle";
import type { ClammDeployment } from "@/lib/clamm/deployment";
import { formatTokenAmount } from "@/lib/clamm/price";
import { P2ID_SALT } from "@/lib/clamm/encoding";

export interface ActivityProps {
  deployment: ClammDeployment;
  activity: ActivityItem[];
  onClaim: (item: ActivityItem) => Promise<unknown>;
  isBusy: boolean;
  error: string | null;
}

const SALT_LABELS: Record<number, string> = {
  [P2ID_SALT.swapOut]: "Swap output",
  [P2ID_SALT.swapRefund]: "Swap refund",
  [P2ID_SALT.mintRefund]: "Mint refund",
  [P2ID_SALT.collect]: "Collect payout",
};

/** Incoming consumable notes (pool P2ID outputs, faucet mints) with a consume action. */
export function Activity({ deployment, activity, onClaim, isBusy, error }: ActivityProps) {
  const tokenLabel = (faucetHex: string, amount: bigint): string => {
    for (const token of [deployment.token0, deployment.token1]) {
      if (token.id === faucetHex) {
        return `${formatTokenAmount(amount, token.decimals)} ${token.symbol}`;
      }
    }
    return `${amount} (unknown token)`;
  };

  return (
    <div className="clamm-card">
      <h3>Incoming notes</h3>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {activity.length === 0 ? (
        <p>Nothing to claim right now.</p>
      ) : (
        <table className="clamm-table">
          <thead>
            <tr>
              <th>Origin</th>
              <th>Assets</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {activity.map((item) => (
              <tr key={item.id} data-testid={`activity-row-${item.id}`}>
                <td>{item.salt !== undefined ? SALT_LABELS[item.salt] : "Transfer"}</td>
                <td>
                  {item.assets.length === 0
                    ? "—"
                    : item.assets.map((a) => tokenLabel(a.faucetHex, a.amount)).join(", ")}
                </td>
                <td>
                  <button type="button" disabled={isBusy} onClick={() => void onClaim(item)}>
                    Claim
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <p className="clamm-hint">
        Claiming consumes the note into your session wallet balance.
      </p>
    </div>
  );
}
