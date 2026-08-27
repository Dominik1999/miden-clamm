import { type ReactNode } from "react";
import { MidenProvider } from "@miden-sdk/react";
import { MidenFiSignerProvider } from "@miden-sdk/miden-wallet-adapter-react";
import { WalletAdapterNetwork } from "@miden-sdk/miden-wallet-adapter-base";
import { APP_NAME, MIDEN_RPC_URL, MIDEN_PROVER } from "@/config";

// v0.15 provider order — MidenProvider runs OUTSIDE the signer provider.
//
// When a signer provider (MidenFiSignerProvider) is an *ancestor* of MidenProvider,
// v0.15 MidenProvider treats it as its external keystore and intentionally does NOT
// create the WebClient until that signer connects (it sees `signerContext.isConnected
// === false` and returns early). With a wallet that hasn't connected — e.g. before the
// user connects, or in any environment without the MidenFi extension — the app would
// then hang forever on "Initializing Miden client…", and even the public counter read
// could never run. (This is undocumented in the migration guide; verified against
// `web-sdk` `packages/react-sdk/src/context/MidenProvider.tsx`.)
//
// This template signs entirely through the local MidenProvider client, not the
// wallet: the increment's two transactions (publish the increment note from a
// throwaway local sender, then consume it as the NoAuth counter) are submitted by
// the WebClient itself (see useIncrementCounter), mirroring the project-template
// `increment_count` reference. So we run MidenProvider in local-keystore mode (no
// signer above it → it initializes immediately and the full increment works
// without a connected wallet) and keep MidenFiSignerProvider *inside* it purely to
// expose a wallet connect button for apps that additionally want wallet-signed
// transactions. MidenFiSignerProvider works standalone (it provides its own
// WalletContext + SignerContext; no MultiSignerProvider required).
export function AppProviders({ children }: { children: ReactNode }) {
  return (
    // `useWorker: false` runs the whole WebClient on one thread.
    //
    // With the default worker shim the client keeps TWO in-memory SMT "forests"
    // (one in the main thread, one in the worker) over the same IndexedDB, and
    // `importAccountById` only registers the MAIN-thread forest while
    // `submitNewTransaction`/`apply_transaction` runs in the WORKER. Incrementing
    // the counter means applying a *delta* transaction to an existing (nonce>0)
    // account we imported rather than created, and that apply path looks the
    // account up in the executing instance's forest — which, under the worker,
    // never contains our late-imported counter, so it fails with
    // "account data wasn't found for account id …". One thread → one forest, so
    // the imported counter is present when the consume is applied. We offload the
    // heavy proving to the remote testnet prover (see useIncrementCounter), so the
    // single thread only pays local execution, not proof generation. (Verified
    // against web-sdk `crates/idxdb-store/src/transaction/mod.rs` apply path.)
    <MidenProvider
      config={{ rpcUrl: MIDEN_RPC_URL, prover: MIDEN_PROVER, useWorker: false }}
      loadingComponent={<div className="loading">Loading Miden WASM...</div>}
    >
      <MidenFiSignerProvider
        appName={APP_NAME}
        network={WalletAdapterNetwork.Testnet}
        autoConnect={false}
      >
        {children}
      </MidenFiSignerProvider>
    </MidenProvider>
  );
}
