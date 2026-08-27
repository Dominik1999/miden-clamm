import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { NoteTracker } from "../NoteTracker";
import { trackedNote } from "./fixtures";
import type { TrackedNote } from "@/lib/clamm/noteStatus";

function renderTracker(overrides: Partial<Parameters<typeof NoteTracker>[0]> = {}) {
  const onReclaim = vi.fn(async (_note: TrackedNote) => undefined);
  render(
    <NoteTracker
      notes={[]}
      currentBlock={1050}
      onReclaim={onReclaim}
      isBusy={false}
      error={null}
      {...overrides}
    />,
  );
  return { onReclaim };
}

describe("NoteTracker", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows the empty state and chain height", () => {
    renderTracker();
    expect(screen.getByText(/No pool notes submitted yet/)).toBeInTheDocument();
    expect(screen.getByText(/Chain height: 1050/)).toBeInTheDocument();
  });

  it("renders each lifecycle status", () => {
    renderTracker({
      notes: [
        trackedNote({ id: "0xa", status: "pending" }),
        trackedNote({ id: "0xb", status: "filled" }),
        trackedNote({ id: "0xc", status: "refunded" }),
        trackedNote({ id: "0xd", status: "reclaimable" }),
        trackedNote({ id: "0xe", status: "reclaimed" }),
        trackedNote({ id: "0xf", kind: "mint", status: "processed" }),
      ],
    });
    expect(screen.getByText("Pending")).toBeInTheDocument();
    expect(screen.getByText("Filled")).toBeInTheDocument();
    expect(screen.getByText("Refunded")).toBeInTheDocument();
    expect(screen.getByText("Reclaimable")).toBeInTheDocument();
    expect(screen.getByText("Reclaimed")).toBeInTheDocument();
    expect(screen.getByText("Processed")).toBeInTheDocument();
  });

  it("offers Reclaim only for reclaimable notes and passes the note through", async () => {
    const reclaimable = trackedNote({ id: "0xd", status: "reclaimable" });
    const { onReclaim } = renderTracker({
      notes: [trackedNote({ id: "0xa", status: "pending" }), reclaimable],
    });
    expect(screen.getAllByRole("button", { name: "Reclaim" })).toHaveLength(1);
    const row = screen.getByTestId("note-row-0xd");
    await userEvent.click(within(row).getByRole("button", { name: "Reclaim" }));
    expect(onReclaim).toHaveBeenCalledTimes(1);
    expect(onReclaim.mock.calls[0][0].id).toBe("0xd");
  });

  it("disables reclaim while busy", () => {
    renderTracker({
      notes: [trackedNote({ id: "0xd", status: "reclaimable" })],
      isBusy: true,
    });
    expect(screen.getByRole("button", { name: "Reclaim" })).toBeDisabled();
  });

  it("renders a dash for notes without a deadline", () => {
    renderTracker({ notes: [trackedNote({ id: "0xf", kind: "collect", deadline: 0, status: "pending" })] });
    expect(screen.getByText("—")).toBeInTheDocument();
  });

  it("surfaces polling errors", () => {
    renderTracker({ error: "sync failed" });
    expect(screen.getByRole("alert")).toHaveTextContent("sync failed");
  });
});
