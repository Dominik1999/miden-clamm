import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { WalletPanel } from "../WalletPanel";
import { DEPLOYMENT, WALLET_ID } from "./fixtures";

function renderPanel(overrides: Partial<Parameters<typeof WalletPanel>[0]> = {}) {
  const onFund = vi.fn(async (_token: "token0" | "token1") => undefined);
  const onRefresh = vi.fn(async () => undefined);
  render(
    <WalletPanel
      deployment={DEPLOYMENT}
      walletId={WALLET_ID}
      balances={{ token0: 1_500_000n, token1: 0n }}
      status={null}
      error={null}
      onFund={onFund}
      onRefresh={onRefresh}
      isBusy={false}
      {...overrides}
    />,
  );
  return { onFund, onRefresh };
}

describe("WalletPanel", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows the wallet id (shortened) and balances", () => {
    renderPanel();
    expect(screen.getByTitle(WALLET_ID)).toBeInTheDocument();
    expect(screen.getByTestId("wallet-balances")).toHaveTextContent("1.5 TKA · 0 TKB");
  });

  it("shows the setup state before the wallet exists", () => {
    renderPanel({ walletId: null, status: "Creating session wallet..." });
    expect(screen.getByText("Creating session wallet...")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Get TKA" })).not.toBeInTheDocument();
  });

  it("funds each token from the dev faucet", async () => {
    const { onFund } = renderPanel();
    await userEvent.click(screen.getByRole("button", { name: "Get TKA" }));
    expect(onFund).toHaveBeenCalledWith("token0");
    await userEvent.click(screen.getByRole("button", { name: "Get TKB" }));
    expect(onFund).toHaveBeenCalledWith("token1");
  });

  it("refreshes balances", async () => {
    const { onRefresh } = renderPanel();
    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });

  it("disables actions while busy and shows progress + errors", () => {
    renderPanel({ isBusy: true, status: "Minting TKA...", error: "faucet key missing" });
    expect(screen.getByRole("button", { name: "Get TKA" })).toBeDisabled();
    expect(screen.getByText("Minting TKA...")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("faucet key missing");
  });
});
