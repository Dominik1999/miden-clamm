import { useMemo, useState } from "react";
import { useDeployment } from "@/hooks/clamm/useDeployment";
import { usePoolState } from "@/hooks/clamm/usePoolState";
import { useClammWallet } from "@/hooks/clamm/useClammWallet";
import { useSubmitPoolNote } from "@/hooks/clamm/useSubmitPoolNote";
import { useNoteLifecycle } from "@/hooks/clamm/useNoteLifecycle";
import { derivePositions } from "@/lib/clamm/store";
import type { TrackedNote } from "@/lib/clamm/noteStatus";
import { PoolView } from "./PoolView";
import { SwapForm } from "./SwapForm";
import { Positions } from "./Positions";
import { NoteTracker } from "./NoteTracker";
import { Activity } from "./Activity";
import { WalletPanel } from "./WalletPanel";
import "./clamm.css";

type Tab = "pool" | "swap" | "positions" | "notes" | "activity";

const TABS: { id: Tab; label: string }[] = [
  { id: "pool", label: "Pool" },
  { id: "swap", label: "Swap" },
  { id: "positions", label: "Positions" },
  { id: "notes", label: "Notes" },
  { id: "activity", label: "Activity" },
];

function parsePositionNote(note: TrackedNote) {
  if (
    (note.kind !== "mint" && note.kind !== "burn") ||
    note.tickLower === undefined ||
    note.tickUpper === undefined ||
    note.liquidity === undefined
  ) {
    return null;
  }
  return {
    lower: note.tickLower,
    upper: note.tickUpper,
    liquidity: BigInt(note.liquidity),
    isBurn: note.kind === "burn",
  };
}

/** The CLAMM pool app: pool view, swap, positions, note tracker, activity. */
export function ClammApp() {
  const { deployment, isLoading: deploymentLoading, error: deploymentError } = useDeployment();
  const wallet = useClammWallet(deployment);
  const pool = usePoolState(deployment);
  const submit = useSubmitPoolNote(deployment, wallet.walletId);
  const lifecycle = useNoteLifecycle(deployment, wallet.walletId);
  const [tab, setTab] = useState<Tab>("pool");

  const positions = useMemo(
    () => derivePositions(lifecycle.notes, parsePositionNote),
    [lifecycle.notes],
  );

  if (deploymentLoading) {
    return <div className="clamm-card">Loading CLAMM deployment...</div>;
  }

  if (deploymentError) {
    return (
      <div className="clamm-card">
        <p className="error" role="alert">
          Invalid deployment descriptor: {deploymentError}
        </p>
      </div>
    );
  }

  if (!deployment) {
    return (
      <div className="clamm-card clamm-setup">
        <h3>CLAMM pool not deployed</h3>
        <p>No deployment descriptor found. To run the pool locally:</p>
        <ol>
          <li>
            Start the local stack:{" "}
            <code>project-template/local-net/start-stack.sh --fresh</code>
          </li>
          <li>
            Export artifacts + deploy:{" "}
            <code>cargo run --bin export_web_artifacts --release -- --deploy</code>
          </li>
          <li>
            Point the frontend at the local network:{" "}
            <code>VITE_MIDEN_RPC_URL=http://localhost:57291</code>{" "}
            <code>VITE_MIDEN_PROVER=http://localhost:50051</code>
          </li>
        </ol>
      </div>
    );
  }

  return (
    <div className="clamm-app">
      <WalletPanel
        deployment={deployment}
        walletId={wallet.walletId}
        balances={wallet.balances}
        status={wallet.status}
        error={wallet.error}
        onFund={wallet.fund}
        onRefresh={wallet.refreshBalances}
        isBusy={wallet.isBusy}
      />

      <nav className="clamm-tabs" aria-label="CLAMM sections">
        {TABS.map(({ id, label }) => (
          <button
            key={id}
            type="button"
            className={tab === id ? "clamm-tab clamm-tab-active" : "clamm-tab"}
            aria-pressed={tab === id}
            onClick={() => setTab(id)}
          >
            {label}
          </button>
        ))}
      </nav>

      {tab === "pool" && (
        <PoolView
          poolState={pool.poolState}
          isLoading={pool.isLoading}
          error={pool.error}
          token0={deployment.token0}
          token1={deployment.token1}
        />
      )}
      {tab === "swap" && (
        <SwapForm
          deployment={deployment}
          poolState={pool.poolState}
          balances={wallet.balances}
          onSubmit={submit.submitSwap}
          stage={submit.stage}
          error={submit.error}
          disabled={!wallet.walletId || wallet.isBusy}
        />
      )}
      {tab === "positions" && (
        <Positions
          deployment={deployment}
          poolState={pool.poolState}
          positions={positions}
          onMint={submit.submitMint}
          onBurn={submit.submitBurn}
          onCollect={submit.submitCollect}
          stage={submit.stage}
          error={submit.error}
          disabled={!wallet.walletId || wallet.isBusy}
        />
      )}
      {tab === "notes" && (
        <NoteTracker
          notes={lifecycle.notes}
          currentBlock={lifecycle.currentBlock}
          onReclaim={lifecycle.reclaim}
          isBusy={lifecycle.isBusy}
          error={lifecycle.error}
        />
      )}
      {tab === "activity" && (
        <Activity
          deployment={deployment}
          activity={lifecycle.activity}
          onClaim={lifecycle.claim}
          isBusy={lifecycle.isBusy}
          error={lifecycle.error}
        />
      )}
    </div>
  );
}
