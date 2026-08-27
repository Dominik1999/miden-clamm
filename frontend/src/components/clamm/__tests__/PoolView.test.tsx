import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { PoolView } from "../PoolView";
import { DEPLOYMENT, POOL_STATE } from "./fixtures";

const base = {
  poolState: POOL_STATE,
  isLoading: false,
  error: null,
  token0: DEPLOYMENT.token0,
  token1: DEPLOYMENT.token1,
};

describe("PoolView", () => {
  it("renders price, tick, liquidity, and fee tier from pool state", () => {
    render(<PoolView {...base} />);
    expect(screen.getByText("TKA / TKB pool")).toBeInTheDocument();
    // sqrtPriceX96 = 2^96 => price exactly 1.0
    expect(screen.getByTestId("pool-price")).toHaveTextContent("1.0000 TKB per TKA");
    expect(screen.getByTestId("pool-tick")).toHaveTextContent("0");
    expect(screen.getByTestId("pool-liquidity")).toHaveTextContent("11000000000000");
    expect(screen.getByTestId("pool-fee")).toHaveTextContent("0.30%");
    expect(screen.getByTestId("pool-block")).toHaveTextContent("1000");
  });

  it("shows a loading state", () => {
    render(<PoolView {...base} poolState={null} isLoading={true} />);
    expect(screen.getByText(/Loading pool state/)).toBeInTheDocument();
  });

  it("shows the error state", () => {
    render(<PoolView {...base} error="pool account 0x9e… not found on-chain" />);
    expect(screen.getByRole("alert")).toHaveTextContent(/Failed to read pool state/);
  });
});
