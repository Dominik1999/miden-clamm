import { describe, it, expect } from "vitest";
import { deriveNoteStatus, NOTE_STATUS_LABELS, type NoteStatusInputs } from "@/lib/clamm/noteStatus";
import { P2ID_SALT } from "@/lib/clamm/encoding";

const base: NoteStatusInputs = {
  kind: "swap",
  consumed: false,
  reclaimedByUser: false,
  currentBlock: 100,
  deadline: 200,
  matchedSalts: [],
};

describe("deriveNoteStatus", () => {
  it("is pending before consumption and deadline", () => {
    expect(deriveNoteStatus(base)).toBe("pending");
  });

  it("is pending when the chain height is unknown", () => {
    expect(deriveNoteStatus({ ...base, currentBlock: null, deadline: 1 })).toBe("pending");
  });

  it("swap consumed with the salt-0 output P2ID is filled", () => {
    expect(
      deriveNoteStatus({ ...base, consumed: true, matchedSalts: [P2ID_SALT.swapOut] }),
    ).toBe("filled");
  });

  it("swap consumed with the salt-1 refund P2ID is refunded", () => {
    expect(
      deriveNoteStatus({ ...base, consumed: true, matchedSalts: [P2ID_SALT.swapRefund] }),
    ).toBe("refunded");
  });

  it("swap consumed with no matched P2ID yet is processed", () => {
    expect(deriveNoteStatus({ ...base, consumed: true })).toBe("processed");
  });

  it("fill takes precedence when both salts matched (cannot co-occur on-chain)", () => {
    expect(
      deriveNoteStatus({
        ...base,
        consumed: true,
        matchedSalts: [P2ID_SALT.swapRefund, P2ID_SALT.swapOut],
      }),
    ).toBe("filled");
  });

  it("mint/burn/collect consumed are processed", () => {
    for (const kind of ["mint", "burn", "collect"] as const) {
      expect(deriveNoteStatus({ ...base, kind, consumed: true })).toBe("processed");
    }
  });

  it("becomes reclaimable exactly at the deadline height", () => {
    expect(deriveNoteStatus({ ...base, currentBlock: 199 })).toBe("pending");
    expect(deriveNoteStatus({ ...base, currentBlock: 200 })).toBe("reclaimable");
    expect(deriveNoteStatus({ ...base, currentBlock: 201 })).toBe("reclaimable");
  });

  it("never becomes reclaimable without a deadline (burn/collect)", () => {
    expect(
      deriveNoteStatus({ ...base, kind: "burn", deadline: 0, currentBlock: 10_000 }),
    ).toBe("pending");
  });

  it("reclaimed by the user wins over everything", () => {
    expect(
      deriveNoteStatus({ ...base, reclaimedByUser: true, consumed: true, currentBlock: 500 }),
    ).toBe("reclaimed");
  });

  it("consumption wins over a passed deadline (pool consumed it just in time)", () => {
    expect(
      deriveNoteStatus({
        ...base,
        consumed: true,
        currentBlock: 500,
        matchedSalts: [P2ID_SALT.swapOut],
      }),
    ).toBe("filled");
  });
});

describe("NOTE_STATUS_LABELS", () => {
  it("labels every status", () => {
    for (const status of [
      "pending",
      "filled",
      "refunded",
      "processed",
      "reclaimable",
      "reclaimed",
    ] as const) {
      expect(NOTE_STATUS_LABELS[status]).toBeTruthy();
    }
  });
});
