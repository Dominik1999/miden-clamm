import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { SwapForm } from "../SwapForm";
import { DEPLOYMENT, POOL_STATE } from "./fixtures";
import type { SwapParams } from "@/hooks/clamm/useSubmitPoolNote";

const balances = { token0: 2_000_000n, token1: 500_000n };

function renderForm(overrides: Partial<Parameters<typeof SwapForm>[0]> = {}) {
  const onSubmit = vi.fn(async (_params: SwapParams) => null);
  render(
    <SwapForm
      deployment={DEPLOYMENT}
      poolState={POOL_STATE}
      balances={balances}
      onSubmit={onSubmit}
      stage="idle"
      error={null}
      {...overrides}
    />,
  );
  return { onSubmit };
}

describe("SwapForm", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("quotes the expected and minimum output from the spot price", async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText(/Amount in/), "1");
    // Price 1.0, fee 0.3% -> quote 0.997; slippage 0.5% -> min out 0.992015.
    expect(screen.getByTestId("swap-quote")).toHaveTextContent(
      "Expected out (spot): 0.997 TKB",
    );
    expect(screen.getByTestId("swap-quote")).toHaveTextContent("Min out: 0.992015 TKB");
  });

  it("submits direction, amount, min out, and deadline from the tip", async () => {
    const { onSubmit } = renderForm();
    await userEvent.type(screen.getByLabelText(/Amount in/), "1");
    await userEvent.click(screen.getByRole("button", { name: "Submit swap" }));
    expect(onSubmit).toHaveBeenCalledTimes(1);
    const params = onSubmit.mock.calls[0][0] as unknown as SwapParams;
    expect(params.direction).toBe(0);
    expect(params.amountIn).toBe(1_000_000n);
    expect(params.minOut).toBe(992_015n); // 997000 * 9950 / 10000
    expect(params.deadline).toBe(1100); // blockHeight 1000 + default 100 blocks
  });

  it("switches direction and uses the other token's balance", async () => {
    const { onSubmit } = renderForm();
    await userEvent.selectOptions(screen.getByLabelText("Direction"), "1");
    expect(screen.getByText(/Balance: 0.5 TKB/)).toBeInTheDocument();
    // Balance is 0.5 TKB; 0.4 is fine.
    await userEvent.type(screen.getByLabelText(/Amount in/), "0.4");
    await userEvent.click(screen.getByRole("button", { name: "Submit swap" }));
    const params = onSubmit.mock.calls[0][0] as unknown as SwapParams;
    expect(params.direction).toBe(1);
    expect(params.amountIn).toBe(400_000n);
  });

  it("blocks submission when the balance is insufficient", async () => {
    const { onSubmit } = renderForm();
    await userEvent.type(screen.getByLabelText(/Amount in/), "3");
    expect(screen.getByRole("alert")).toHaveTextContent(/Insufficient TKA balance/);
    expect(screen.getByRole("button", { name: "Submit swap" })).toBeDisabled();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("rejects an invalid slippage", async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText(/Amount in/), "1");
    await userEvent.clear(screen.getByLabelText(/Slippage/));
    await userEvent.type(screen.getByLabelText(/Slippage/), "101");
    expect(screen.getByRole("alert")).toHaveTextContent(/Slippage must be between/);
    expect(screen.getByRole("button", { name: "Submit swap" })).toBeDisabled();
  });

  it("rejects an invalid deadline", async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText(/Amount in/), "1");
    await userEvent.clear(screen.getByLabelText(/Deadline/));
    await userEvent.type(screen.getByLabelText(/Deadline/), "0");
    expect(screen.getByRole("alert")).toHaveTextContent(/Deadline must be a positive/);
  });

  it("rejects a malformed amount", async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText(/Amount in/), "abc");
    expect(screen.getByRole("alert")).toHaveTextContent(/invalid amount/);
  });

  it("shows the submit stage and disables while in flight", () => {
    renderForm({ stage: "submitting" });
    expect(screen.getByRole("button", { name: "Submitting..." })).toBeDisabled();
  });

  it("surfaces hook errors", () => {
    renderForm({ error: "prover unreachable" });
    expect(screen.getByRole("alert")).toHaveTextContent("prover unreachable");
  });

  it("disables submission without pool state", async () => {
    renderForm({ poolState: null });
    await userEvent.type(screen.getByLabelText(/Amount in/), "1");
    expect(screen.getByRole("button", { name: "Submit swap" })).toBeDisabled();
  });
});
