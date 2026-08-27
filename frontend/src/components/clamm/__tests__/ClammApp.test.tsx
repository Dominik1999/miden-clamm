import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";

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
import { DEPLOYMENT, POOL_STATE, WALLET_ID, trackedNote } from "./fixtures";

function mockHooks(overrides: { deployment?: typeof DEPLOYMENT | null } = {}) {
  vi.mocked(useDeployment).mockReturnValue({
    deployment: overrides.deployment === undefined ? DEPLOYMENT : overrides.deployment,
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
    notes: [
      trackedNote({ id: "0xmint1", kind: "mint", tickLower: -120, tickUpper: 120, liquidity: "5000", status: "processed" }),
    ],
    activity: [],
    currentBlock: 1050,
    isBusy: false,
    error: null,
    refresh: vi.fn(async () => undefined),
    reclaim: vi.fn(async () => undefined),
    claim: vi.fn(async () => undefined),
  });
}

describe("ClammApp", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockHooks();
  });

  it("shows deploy instructions when no deployment descriptor exists", () => {
    mockHooks({ deployment: null });
    render(<ClammApp />);
    expect(screen.getByText("CLAMM pool not deployed")).toBeInTheDocument();
    expect(screen.getByText(/export_web_artifacts/)).toBeInTheDocument();
  });

  it("shows the loading state while the descriptor loads", () => {
    vi.mocked(useDeployment).mockReturnValue({ deployment: null, isLoading: true, error: null });
    render(<ClammApp />);
    expect(screen.getByText(/Loading CLAMM deployment/)).toBeInTheDocument();
  });

  it("shows a malformed-descriptor error", () => {
    vi.mocked(useDeployment).mockReturnValue({
      deployment: null,
      isLoading: false,
      error: "deployment.pool.id is not a valid account id",
    });
    render(<ClammApp />);
    expect(screen.getByRole("alert")).toHaveTextContent(/Invalid deployment descriptor/);
  });

  it("renders the wallet panel and the pool tab by default", () => {
    render(<ClammApp />);
    expect(screen.getByText("Session wallet")).toBeInTheDocument();
    expect(screen.getByText("TKA / TKB pool")).toBeInTheDocument();
  });

  it("switches between tabs", async () => {
    render(<ClammApp />);
    await userEvent.click(screen.getByRole("button", { name: "Swap" }));
    expect(screen.getByRole("form", { name: "Swap" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Positions" }));
    expect(screen.getByText("Add liquidity")).toBeInTheDocument();
    // Position derived from the tracked mint note.
    expect(screen.getByText("[-120, 120]")).toBeInTheDocument();
    expect(screen.getByText("5000")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Notes" }));
    expect(screen.getByText("Submitted notes")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Activity" }));
    expect(screen.getByText("Incoming notes")).toBeInTheDocument();
  });
});
