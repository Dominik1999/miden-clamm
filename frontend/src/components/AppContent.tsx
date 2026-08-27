import { useMiden, useSyncState } from "@miden-sdk/react";
import { useMidenFiWallet } from "@miden-sdk/miden-wallet-adapter-react";
import { WalletReadyState } from "@miden-sdk/miden-wallet-adapter-base";
import reactLogo from "@/assets/react.svg";
import midenLogo from "@/assets/miden.svg";
import viteLogo from "/vite.svg";
import { Counter } from "@/components/Counter";
import { ClammApp } from "@/components/clamm/ClammApp";
import "./AppContent.css";

function WalletButton() {
  // Use the MidenFi-specific hook (not the generic `useSigner()`) so we can
  // gate on `wallet.readyState`. `useSigner().connect()` calls through to the
  // same provider, but at the moment the user clicks the button the adapter
  // may not yet have detected `window.midenWallet` — detection is polled, see
  // `scopePollingDetectionStrategy` in @miden-sdk/miden-wallet-adapter-base.
  // When readyState is NotDetected, MidenFiSignerProvider falls back to
  // `window.open(adapter.url, "_blank")` — the Chrome Web Store URL — which
  // on some platforms redirects to the Play Store. Disabling the button
  // until the extension is detected prevents the fallback from firing.
  const { wallet, connected, connecting, connect, disconnect } =
    useMidenFiWallet();
  const readyState = wallet?.readyState;
  const walletReady =
    readyState === WalletReadyState.Installed ||
    readyState === WalletReadyState.Loadable;

  if (!walletReady) {
    return <button disabled>Install MidenFi Wallet</button>;
  }
  if (connected) {
    return <button onClick={disconnect}>Disconnect Wallet</button>;
  }
  if (connecting) {
    return <button disabled>Connecting...</button>;
  }
  return <button onClick={connect}>Connect Wallet</button>;
}

export function AppContent() {
  const { isReady, isInitializing, error } = useMiden();
  const { syncHeight } = useSyncState();

  if (error) {
    return (
      <div className="loading">
        <p>Failed to initialize Miden client</p>
        <p className="error">{error.message}</p>
      </div>
    );
  }

  if (isInitializing || !isReady) {
    return <div className="loading">Initializing Miden client...</div>;
  }

  return (
    <>
      <div>
        <a href="https://vite.dev" target="_blank" rel="noreferrer">
          <img src={viteLogo} className="logo" alt="Vite logo" />
        </a>
        <a href="https://react.dev" target="_blank" rel="noreferrer">
          <img src={reactLogo} className="logo react" alt="React logo" />
        </a>
        <a href="https://docs.miden.xyz" target="_blank" rel="noreferrer">
          <img src={midenLogo} className="logo miden" alt="Miden logo" />
        </a>
      </div>
      <h1>Vite + React + Miden</h1>
      <div className="wallet-section">
        <WalletButton />
      </div>
      <ClammApp />
      <Counter />
      <p className="read-the-docs">
        Testnet block: {syncHeight ?? "syncing..."} | Click on the Vite, React,
        and Miden logos to learn more
      </p>
    </>
  );
}
