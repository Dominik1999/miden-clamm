import { render, screen } from "@testing-library/react";
import { vi, describe, it, expect, beforeEach } from "vitest";

vi.mock("@miden-sdk/react", () => import("@/__tests__/mocks/miden-sdk-react"));
vi.mock("@miden-sdk/miden-wallet-adapter-react", () => ({
  useMidenFiWallet: vi.fn(() => ({
    autoConnect: false,
    wallets: [],
    wallet: null,
    address: null,
    publicKey: null,
    connected: false,
    connecting: false,
    disconnecting: false,
    select: vi.fn(),
    connect: vi.fn(async () => undefined),
    disconnect: vi.fn(async () => undefined),
    requestTransaction: vi.fn(async () => "0xtx"),
    requestAssets: undefined,
    requestPrivateNotes: undefined,
    signBytes: undefined,
    importPrivateNote: undefined,
    requestConsumableNotes: undefined,
    waitForTransaction: undefined,
    requestSend: undefined,
    requestConsume: undefined,
    createAccount: undefined,
  })),
}));
vi.mock("@miden-sdk/miden-wallet-adapter-base", () => ({
  WalletReadyState: {
    Installed: "Installed",
    NotDetected: "NotDetected",
    Loadable: "Loadable",
    Unsupported: "Unsupported",
  },
}));
vi.mock("@/components/Counter", () => ({
  Counter: () => <div data-testid="counter">Counter Mock</div>,
}));
vi.mock("@/components/clamm/ClammApp", () => ({
  ClammApp: () => <div data-testid="clamm-app">Clamm Mock</div>,
}));

import { useMiden, useSyncState } from "@miden-sdk/react";
import { useMidenFiWallet } from "@miden-sdk/miden-wallet-adapter-react";
import userEvent from "@testing-library/user-event";
import { AppContent } from "../AppContent";

type WalletState = ReturnType<typeof useMidenFiWallet>;
type WalletInner = NonNullable<WalletState["wallet"]>;

function walletState(
  overrides: Partial<{
    readyState: "Installed" | "NotDetected" | "Loadable" | "Unsupported";
    connected: boolean;
    connecting: boolean;
    address: string | null;
    connect: () => Promise<void>;
    disconnect: () => Promise<void>;
    requestTransaction: WalletState["requestTransaction"];
  }> = {},
): WalletState {
  const {
    readyState = "Installed",
    connected = false,
    connecting = false,
    address = connected ? "mtst1arwk88k8smzcq5p30upr6eerw5npmnyz" : null,
    connect = vi.fn(async () => undefined),
    disconnect = vi.fn(async () => undefined),
    requestTransaction = vi.fn(async () => "0xtx"),
  } = overrides;
  // Build a shape that satisfies WalletContextState; the inner `Wallet`
  // (`{ adapter, readyState }`) shape requires an Adapter, which we stub
  // with a structural cast since the component only reads `readyState`.
  const innerWallet = {
    adapter: {} as WalletInner["adapter"],
    readyState,
  } as WalletInner;
  return {
    autoConnect: false,
    wallets: [innerWallet],
    wallet: innerWallet,
    address,
    publicKey: null,
    connected,
    connecting,
    disconnecting: false,
    select: vi.fn(),
    connect,
    disconnect,
    requestTransaction,
    requestAssets: undefined,
    requestPrivateNotes: undefined,
    signBytes: undefined,
    importPrivateNote: undefined,
    requestConsumableNotes: undefined,
    waitForTransaction: undefined,
    requestSend: undefined,
    requestConsume: undefined,
    createAccount: undefined,
  };
}

const midenReady = {
  client: null,
  isReady: true,
  isInitializing: false,
  error: null,
  sync: vi.fn(),
  runExclusive: vi.fn(),
  prover: null,
  signerAccountId: null,
  signerConnected: null,
};

describe("AppContent", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    // Restore default ready state after any test that overrides useMiden
    vi.mocked(useMiden).mockReturnValue(midenReady);
    vi.mocked(useSyncState).mockReturnValue({
      syncHeight: 12345,
      isSyncing: false,
      lastSyncTime: Date.now(),
      error: null,
      sync: vi.fn(),
    });
    vi.mocked(useMidenFiWallet).mockReturnValue(
      walletState({ readyState: "NotDetected" }),
    );
  });

  it("renders main content when Miden is ready", () => {
    render(<AppContent />);

    expect(screen.getByText("Vite + React + Miden")).toBeInTheDocument();
    expect(screen.getByAltText("Vite logo")).toBeInTheDocument();
    expect(screen.getByAltText("React logo")).toBeInTheDocument();
    expect(screen.getByAltText("Miden logo")).toBeInTheDocument();
    expect(screen.getByTestId("counter")).toBeInTheDocument();
    expect(screen.getByTestId("clamm-app")).toBeInTheDocument();
  });

  it("shows sync height from testnet", () => {
    render(<AppContent />);
    expect(screen.getByText(/Testnet block: 12345/)).toBeInTheDocument();
  });

  it("shows syncing indicator when syncHeight is null", () => {
    vi.mocked(useSyncState).mockReturnValue({
      syncHeight: null as unknown as number,
      isSyncing: true,
      lastSyncTime: null,
      error: null,
      sync: vi.fn(),
    });

    render(<AppContent />);
    expect(screen.getByText(/syncing\.\.\./)).toBeInTheDocument();
  });

  it("shows loading message during initialization", () => {
    vi.mocked(useMiden).mockReturnValue({
      client: null,
      isReady: false,
      isInitializing: true,
      error: null,
      sync: vi.fn(),
      runExclusive: vi.fn(),
      prover: null,
      signerAccountId: null,
      signerConnected: null,
    });

    render(<AppContent />);
    expect(
      screen.getByText("Initializing Miden client..."),
    ).toBeInTheDocument();
    expect(screen.queryByText("Vite + React + Miden")).not.toBeInTheDocument();
  });

  it("shows disabled install-wallet button when extension is not detected", () => {
    render(<AppContent />);
    const button = screen.getByRole("button", { name: "Install MidenFi Wallet" });
    expect(button).toBeDisabled();
  });

  it("shows connect button when wallet is installed and disconnected", () => {
    vi.mocked(useMidenFiWallet).mockReturnValue(
      walletState({ readyState: "Installed", connected: false }),
    );

    render(<AppContent />);
    expect(
      screen.getByRole("button", { name: "Connect Wallet" }),
    ).toBeEnabled();
  });

  it("shows disconnect button when wallet is connected", () => {
    vi.mocked(useMidenFiWallet).mockReturnValue(
      walletState({ readyState: "Installed", connected: true }),
    );

    render(<AppContent />);
    expect(
      screen.getByRole("button", { name: "Disconnect Wallet" }),
    ).toBeInTheDocument();
  });

  it("shows connecting state while the wallet request is in flight", () => {
    vi.mocked(useMidenFiWallet).mockReturnValue(
      walletState({
        readyState: "Installed",
        connected: false,
        connecting: true,
      }),
    );

    render(<AppContent />);
    const button = screen.getByRole("button", { name: /Connecting/ });
    expect(button).toBeDisabled();
  });

  it("calls connect on wallet button click", async () => {
    const mockConnect = vi.fn(async () => undefined);
    vi.mocked(useMidenFiWallet).mockReturnValue(
      walletState({
        readyState: "Installed",
        connected: false,
        connect: mockConnect,
      }),
    );

    render(<AppContent />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Connect Wallet" }));
    expect(mockConnect).toHaveBeenCalledOnce();
  });

  it("calls disconnect on wallet button click", async () => {
    const mockDisconnect = vi.fn(async () => undefined);
    vi.mocked(useMidenFiWallet).mockReturnValue(
      walletState({
        readyState: "Installed",
        connected: true,
        disconnect: mockDisconnect,
      }),
    );

    render(<AppContent />);
    const user = userEvent.setup();
    await user.click(
      screen.getByRole("button", { name: "Disconnect Wallet" }),
    );
    expect(mockDisconnect).toHaveBeenCalledOnce();
  });

  it("shows error message on initialization failure", () => {
    vi.mocked(useMiden).mockReturnValue({
      client: null,
      isReady: false,
      isInitializing: false,
      error: new Error("WASM failed to load"),
      sync: vi.fn(),
      runExclusive: vi.fn(),
      prover: null,
      signerAccountId: null,
      signerConnected: null,
    });

    render(<AppContent />);
    expect(
      screen.getByText("Failed to initialize Miden client"),
    ).toBeInTheDocument();
    expect(screen.getByText("WASM failed to load")).toBeInTheDocument();
  });
});
