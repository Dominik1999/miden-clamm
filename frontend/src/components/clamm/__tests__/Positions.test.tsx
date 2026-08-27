import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { Positions } from "../Positions";
import { DEPLOYMENT, POOL_STATE } from "./fixtures";
import type { MintParams, BurnParams, CollectParams } from "@/hooks/clamm/useSubmitPoolNote";
import type { TrackedPosition } from "@/lib/clamm/store";

const positions: TrackedPosition[] = [
  { tickLower: -120, tickUpper: 120, liquidity: "1000000000000" },
  { tickLower: -240, tickUpper: -120, liquidity: "0" },
];

function renderPositions(overrides: Partial<Parameters<typeof Positions>[0]> = {}) {
  const onMint = vi.fn(async (_p: MintParams) => null);
  const onBurn = vi.fn(async (_p: BurnParams) => null);
  const onCollect = vi.fn(async (_p: CollectParams) => null);
  render(
    <Positions
      deployment={DEPLOYMENT}
      poolState={POOL_STATE}
      positions={positions}
      onMint={onMint}
      onBurn={onBurn}
      onCollect={onCollect}
      stage="idle"
      error={null}
      {...overrides}
    />,
  );
  return { onMint, onBurn, onCollect };
}

describe("Positions mint form", () => {
  beforeEach(() => vi.clearAllMocks());

  it("submits a valid mint with computed deadline", async () => {
    const { onMint } = renderPositions();
    await userEvent.type(screen.getByLabelText(/Liquidity/), "1000000000000");
    await userEvent.type(screen.getByLabelText(/Max TKA deposit/), "1");
    await userEvent.type(screen.getByLabelText(/Max TKB deposit/), "2");
    await userEvent.click(screen.getByRole("button", { name: "Mint position" }));
    expect(onMint).toHaveBeenCalledTimes(1);
    const params = onMint.mock.calls[0][0];
    expect(params).toEqual({
      tickLower: -120,
      tickUpper: 120,
      liquidity: 1_000_000_000_000n,
      amount0Max: 1_000_000n,
      amount1Max: 2_000_000n,
      deadline: 1100, // blockHeight 1000 + 100
    });
  });

  it("rejects misaligned ticks (tick spacing 60)", async () => {
    const { onMint } = renderPositions();
    await userEvent.clear(screen.getByLabelText(/Tick lower/));
    await userEvent.type(screen.getByLabelText(/Tick lower/), "-121");
    await userEvent.type(screen.getByLabelText(/Liquidity/), "1000");
    await userEvent.type(screen.getByLabelText(/Max TKA deposit/), "1");
    await userEvent.click(screen.getByRole("button", { name: "Mint position" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/multiples of the pool tick spacing/);
    expect(onMint).not.toHaveBeenCalled();
  });

  it("rejects ticks outside ±443,636", async () => {
    const { onMint } = renderPositions();
    await userEvent.clear(screen.getByLabelText(/Tick upper/));
    await userEvent.type(screen.getByLabelText(/Tick upper/), "443700");
    await userEvent.type(screen.getByLabelText(/Liquidity/), "1000");
    await userEvent.type(screen.getByLabelText(/Max TKA deposit/), "1");
    await userEvent.click(screen.getByRole("button", { name: "Mint position" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/443,636/);
    expect(onMint).not.toHaveBeenCalled();
  });

  it("rejects an inverted range", async () => {
    const { onMint } = renderPositions();
    await userEvent.clear(screen.getByLabelText(/Tick lower/));
    await userEvent.type(screen.getByLabelText(/Tick lower/), "180");
    await userEvent.type(screen.getByLabelText(/Liquidity/), "1000");
    await userEvent.type(screen.getByLabelText(/Max TKA deposit/), "1");
    await userEvent.click(screen.getByRole("button", { name: "Mint position" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/Lower tick must be below/);
    expect(onMint).not.toHaveBeenCalled();
  });

  it("requires liquidity and at least one deposit amount", async () => {
    const { onMint } = renderPositions();
    await userEvent.type(screen.getByLabelText(/Liquidity/), "1000");
    await userEvent.click(screen.getByRole("button", { name: "Mint position" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/at least one max deposit/);
    expect(onMint).not.toHaveBeenCalled();
  });

  it("rejects non-integer liquidity", async () => {
    const { onMint } = renderPositions();
    await userEvent.type(screen.getByLabelText(/Liquidity/), "1.5");
    await userEvent.type(screen.getByLabelText(/Max TKA deposit/), "1");
    await userEvent.click(screen.getByRole("button", { name: "Mint position" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/positive integer/);
    expect(onMint).not.toHaveBeenCalled();
  });

  it("disables minting while a submission is in flight", () => {
    renderPositions({ stage: "submitting" });
    expect(screen.getByRole("button", { name: "Working..." })).toBeDisabled();
  });
});

describe("Positions list", () => {
  beforeEach(() => vi.clearAllMocks());

  it("renders tracked positions with their liquidity", () => {
    renderPositions();
    expect(screen.getByText("[-120, 120]")).toBeInTheDocument();
    expect(screen.getByText("1000000000000")).toBeInTheDocument();
    expect(screen.getByText("[-240, -120]")).toBeInTheDocument();
  });

  it("burns the full tracked liquidity for a range", async () => {
    const { onBurn } = renderPositions();
    const row = screen.getByText("[-120, 120]").closest("tr")!;
    await userEvent.click(within(row).getByRole("button", { name: "Burn" }));
    expect(onBurn).toHaveBeenCalledWith({
      tickLower: -120,
      tickUpper: 120,
      liquidity: 1_000_000_000_000n,
    });
  });

  it("disables burn for zero-liquidity positions but still allows collect", async () => {
    const { onCollect } = renderPositions();
    const row = screen.getByText("[-240, -120]").closest("tr")!;
    expect(within(row).getByRole("button", { name: "Burn" })).toBeDisabled();
    await userEvent.click(within(row).getByRole("button", { name: "Collect" }));
    expect(onCollect).toHaveBeenCalledWith({ tickLower: -240, tickUpper: -120 });
  });

  it("shows the empty state", () => {
    renderPositions({ positions: [] });
    expect(screen.getByText(/No positions minted from this browser yet/)).toBeInTheDocument();
  });
});
