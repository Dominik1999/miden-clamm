import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { Activity } from "../Activity";
import { DEPLOYMENT, activityItem } from "./fixtures";
import type { ActivityItem } from "@/hooks/clamm/useNoteLifecycle";

function renderActivity(overrides: Partial<Parameters<typeof Activity>[0]> = {}) {
  const onClaim = vi.fn(async (_item: ActivityItem) => undefined);
  render(
    <Activity
      deployment={DEPLOYMENT}
      activity={[]}
      onClaim={onClaim}
      isBusy={false}
      error={null}
      {...overrides}
    />,
  );
  return { onClaim };
}

describe("Activity", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows the empty state", () => {
    renderActivity();
    expect(screen.getByText(/Nothing to claim right now/)).toBeInTheDocument();
  });

  it("labels pool P2ID outputs by their derivation salt", () => {
    renderActivity({
      activity: [
        activityItem({ id: "0x1", salt: 0 }),
        activityItem({ id: "0x2", salt: 1 }),
        activityItem({ id: "0x3", salt: 2 }),
        activityItem({ id: "0x4", salt: 3 }),
        activityItem({ id: "0x5", salt: undefined, sourceNoteId: undefined }),
      ],
    });
    expect(screen.getByText("Swap output")).toBeInTheDocument();
    expect(screen.getByText("Swap refund")).toBeInTheDocument();
    expect(screen.getByText("Mint refund")).toBeInTheDocument();
    expect(screen.getByText("Collect payout")).toBeInTheDocument();
    expect(screen.getByText("Transfer")).toBeInTheDocument();
  });

  it("formats known token assets and flags unknown faucets", () => {
    renderActivity({
      activity: [
        activityItem({ id: "0x1", assets: [{ faucetHex: DEPLOYMENT.token1.id, amount: 997_000n }] }),
        activityItem({ id: "0x2", assets: [{ faucetHex: "0xdead", amount: 5n }], salt: undefined }),
      ],
    });
    expect(screen.getByText("0.997 TKB")).toBeInTheDocument();
    expect(screen.getByText(/unknown token/)).toBeInTheDocument();
  });

  it("claims a note", async () => {
    const { onClaim } = renderActivity({ activity: [activityItem({ id: "0xp2id1" })] });
    const row = screen.getByTestId("activity-row-0xp2id1");
    await userEvent.click(within(row).getByRole("button", { name: "Claim" }));
    expect(onClaim).toHaveBeenCalledTimes(1);
    expect(onClaim.mock.calls[0][0].id).toBe("0xp2id1");
  });

  it("disables claiming while busy and surfaces errors", () => {
    renderActivity({ activity: [activityItem()], isBusy: true, error: "consume failed" });
    expect(screen.getByRole("button", { name: "Claim" })).toBeDisabled();
    expect(screen.getByRole("alert")).toHaveTextContent("consume failed");
  });
});
