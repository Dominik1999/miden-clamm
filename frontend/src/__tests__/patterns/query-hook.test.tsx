/**
 * TEST PATTERN: Query Hook Component
 *
 * Shows how to test a component that displays data from Miden query hooks.
 * Covers the three essential states: loading, success (with data), and error.
 *
 * Key concepts:
 * - Override hook return values per-test with vi.mocked()
 * - Test loading skeleton/placeholder states
 * - Test data rendering with realistic fixtures
 * - Test error display and recovery (refetch)
 */

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { vi, describe, it, expect, beforeEach } from "vitest";

vi.mock("@miden-sdk/react", () => import("@/__tests__/mocks/miden-sdk-react"));

import { useAccounts, useSyncState } from "@miden-sdk/react";
import {
  MOCK_WALLET_HEADER,
  MOCK_WALLET_HEADER_2,
  MOCK_FAUCET_HEADER,
} from "@/__tests__/fixtures";

// Example component that lists accounts — a common Miden UI pattern.
// v0.15: useAccounts() returns `accounts` (the source of truth). `wallets` is
// @deprecated (mirrors `accounts`) and `faucets` is @deprecated and ALWAYS EMPTY
// (the faucet-vs-wallet flag was removed from the account id; detect faucets
// per-account from their components). So this pattern uses `accounts`.
function AccountList() {
  const { accounts, isLoading, error, refetch } = useAccounts();
  const { syncHeight } = useSyncState();

  if (error) {
    return (
      <div>
        <p role="alert">Failed to load accounts: {error.message}</p>
        <button onClick={refetch}>Retry</button>
      </div>
    );
  }

  if (isLoading) {
    return <p>Loading accounts...</p>;
  }

  return (
    <div>
      <p>Synced to block {syncHeight}</p>
      <h2>Accounts ({accounts.length})</h2>
      <ul aria-label="accounts">
        {accounts.map((a) => (
          <li key={String(a.id)}>{String(a.id)}</li>
        ))}
      </ul>
    </div>
  );
}

describe("Query Hook Pattern", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  // Default mocks return realistic data — component should render the account list
  it("renders the account list with data", () => {
    render(<AccountList />);

    // `accounts` is the v0.15 source of truth — every account header appears here
    const accountList = screen.getByRole("list", { name: "accounts" });
    expect(accountList.children).toHaveLength(3);
    expect(screen.getByText(MOCK_WALLET_HEADER.id)).toBeInTheDocument();
    expect(screen.getByText(MOCK_WALLET_HEADER_2.id)).toBeInTheDocument();
    expect(screen.getByText(MOCK_FAUCET_HEADER.id)).toBeInTheDocument();

    // Sync height from useSyncState mock
    expect(screen.getByText(/Synced to block 12345/)).toBeInTheDocument();
  });

  // Override to loading state — component should show loading indicator
  it("shows loading state while fetching accounts", () => {
    vi.mocked(useAccounts).mockReturnValue({
      accounts: [],
      wallets: [],
      faucets: [],
      isLoading: true,
      error: null,
      refetch: vi.fn(),
    });

    render(<AccountList />);
    expect(screen.getByText("Loading accounts...")).toBeInTheDocument();
    // Account lists should NOT be rendered during loading
    expect(screen.queryByRole("list")).not.toBeInTheDocument();
  });

  // Override to error state — component should show error with retry button
  it("shows error with retry button on failure", async () => {
    const mockRefetch = vi.fn();
    vi.mocked(useAccounts).mockReturnValue({
      accounts: [],
      wallets: [],
      faucets: [],
      isLoading: false,
      error: new Error("Network timeout"),
      refetch: mockRefetch,
    });

    render(<AccountList />);

    // Error message should be visible and accessible
    expect(screen.getByRole("alert")).toHaveTextContent("Network timeout");

    // Clicking retry should call refetch
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(mockRefetch).toHaveBeenCalledOnce();
  });

  // Test empty state — no accounts yet (fresh install)
  it("renders empty lists when no accounts exist", () => {
    vi.mocked(useAccounts).mockReturnValue({
      accounts: [],
      wallets: [],
      faucets: [],
      isLoading: false,
      error: null,
      refetch: vi.fn(),
    });

    render(<AccountList />);
    expect(screen.getByText("Accounts (0)")).toBeInTheDocument();
  });
});
