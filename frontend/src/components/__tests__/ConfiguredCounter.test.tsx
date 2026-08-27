import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { vi, describe, it, expect, beforeEach } from "vitest";

vi.mock("@/hooks/useIncrementCounter", () => ({
  useIncrementCounter: vi.fn(),
}));

import { useIncrementCounter } from "@/hooks/useIncrementCounter";
import { ConfiguredCounter } from "../ConfiguredCounter";

const FIXTURE_ADDRESS = "0xdeadbeef00000001";

const defaultHookReturn = {
  increment: vi.fn(),
  count: 42 as number | null,
  isSubmitting: false,
  status: null as string | null,
  error: null as string | null,
  explorerUrl: `https://testnet.midenscan.com/account/${FIXTURE_ADDRESS}`,
};

describe("ConfiguredCounter", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(useIncrementCounter).mockReturnValue(defaultHookReturn);
  });

  it("displays the current count on the button", () => {
    render(<ConfiguredCounter counterAddress={FIXTURE_ADDRESS} />);
    expect(
      screen.getByRole("button", { name: "count is 42" }),
    ).toBeInTheDocument();
  });

  it("calls increment on button click", async () => {
    const mockIncrement = vi.fn();
    vi.mocked(useIncrementCounter).mockReturnValue({
      ...defaultHookReturn,
      increment: mockIncrement,
    });

    render(<ConfiguredCounter counterAddress={FIXTURE_ADDRESS} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "count is 42" }));
    expect(mockIncrement).toHaveBeenCalledOnce();
  });

  it("shows the current step and disables the button while incrementing", () => {
    vi.mocked(useIncrementCounter).mockReturnValue({
      ...defaultHookReturn,
      isSubmitting: true,
      status: "Incrementing (consuming note)...",
    });

    render(<ConfiguredCounter counterAddress={FIXTURE_ADDRESS} />);
    const button = screen.getByRole("button", {
      name: "Incrementing (consuming note)...",
    });
    expect(button).toBeDisabled();
  });

  it("disables the button while the count is loading (null)", () => {
    vi.mocked(useIncrementCounter).mockReturnValue({
      ...defaultHookReturn,
      count: null,
    });

    render(<ConfiguredCounter counterAddress={FIXTURE_ADDRESS} />);
    const button = screen.getByRole("button", { name: "count is ..." });
    expect(button).toBeDisabled();
  });

  it("displays error message", () => {
    vi.mocked(useIncrementCounter).mockReturnValue({
      ...defaultHookReturn,
      error: "Transaction failed",
    });

    render(<ConfiguredCounter counterAddress={FIXTURE_ADDRESS} />);
    expect(screen.getByText("Transaction failed")).toBeInTheDocument();
  });

  it("links to explorer with counter address", () => {
    render(<ConfiguredCounter counterAddress={FIXTURE_ADDRESS} />);
    const link = screen.getByRole("link");
    expect(link).toHaveAttribute(
      "href",
      `https://testnet.midenscan.com/account/${FIXTURE_ADDRESS}`,
    );
    expect(link).toHaveAttribute("target", "_blank");
  });
});
