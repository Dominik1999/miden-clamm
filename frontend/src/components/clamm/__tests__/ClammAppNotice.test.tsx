// Verifies the persistent "network is not servicing pool notes" honesty
// notice (CLAMM_NTX_PASSIVE, set via VITE_CLAMM_NTX_PASSIVE for the public
// testnet build): shown on every tab, including the note-submitting swap and
// positions flows, while pool reads keep rendering.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@/config", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/config")>()),
  CLAMM_NTX_PASSIVE: true,
}));
vi.mock("@/hooks/clamm/useDeployment", () => ({ useDeployment: vi.fn() }));
vi.mock("@/hooks/clamm/usePoolState", () => ({ usePoolState: vi.fn() }));
vi.mock("@/hooks/clamm/useClammWallet", () => ({ useClammWallet: vi.fn() }));
vi.mock("@/hooks/clamm/useSubmitPoolNote", () => ({ useSubmitPoolNote: vi.fn() }));
vi.mock("@/hooks/clamm/useNoteLifecycle", () => ({ useNoteLifecycle: vi.fn() }));

import { useDeployment } from "@/hooks/clamm/useDeployment";
import { usePoolState } from "@/hooks/clamm/usePoolState";
import { useClammWallet } from "@/hooks/clamm/useClammWallet";
import { useSubmitPoolNote } from "@/hooks/clamm/useSubmitPoolNote";
import { useNoteLifecycle } from "@/hooks/clamm/useNoteLifecycle";
import { ClammApp } from "../ClammApp";
import { DEPLOYMENT, POOL_STATE, WALLET_ID } from "./fixtures";

function mockHooks() {
  vi.mocked(useDeployment).mockReturnValue({
    deployment: DEPLOYMENT,
    isLoading: false,
    error: null,
  });
  vi.mocked(usePoolState).mockReturnValue({
    poolState: POOL_STATE,
    isLoading: false,
    error: null,
    refresh: vi.fn(async () => undefined),
  });
  vi.mocked(useClammWallet).mockReturnValue({
    walletId: WALLET_ID,
    balances: { token0: 1_000_000n, token1: 2_000_000n },
    isBusy: false,
    status: null,
    error: null,
    ensureWallet: vi.fn(async () => undefined),
    fund: vi.fn(async () => undefined),
    refreshBalances: vi.fn(async () => undefined),
  });
  vi.mocked(useSubmitPoolNote).mockReturnValue({
    stage: "idle",
    error: null,
    isLoading: false,
    reset: vi.fn(),
    submitSwap: vi.fn(async () => null),
    submitMint: vi.fn(async () => null),
    submitBurn: vi.fn(async () => null),
    submitCollect: vi.fn(async () => null),
  });
  vi.mocked(useNoteLifecycle).mockReturnValue({
    notes: [],
    activity: [],
    currentBlock: 1050,
    isBusy: false,
    error: null,
    refresh: vi.fn(async () => undefined),
    reclaim: vi.fn(async () => undefined),
    claim: vi.fn(async () => undefined),
  });
}

describe("ClammApp ntx-passive notice", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockHooks();
  });

  it("shows the persistent notice with the reclaim guidance", () => {
    render(<ClammApp />);
    const notice = screen.getByTestId("ntx-passive-notice");
    expect(notice).toHaveTextContent(
      "Miden testnet is not currently executing pool operations",
    );
    expect(notice).toHaveTextContent(/sit pending/);
    expect(notice).toHaveTextContent(/reclaim any pending note/i);
  });

  it("keeps the notice visible on the swap flow while pool reads still render", async () => {
    render(<ClammApp />);
    // Pool reads still work.
    expect(screen.getByText("TKA / TKB pool")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Swap" }));
    expect(screen.getByRole("form", { name: "Swap" })).toBeInTheDocument();
    expect(screen.getByTestId("ntx-passive-notice")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Positions" }));
    expect(screen.getByText("Add liquidity")).toBeInTheDocument();
    expect(screen.getByTestId("ntx-passive-notice")).toBeInTheDocument();
  });
});
