import { describe, it, expect, beforeEach } from "vitest";
import {
  loadTrackedNotes,
  saveTrackedNotes,
  addTrackedNote,
  loadWalletId,
  saveWalletId,
  loadReclaimedNoteIds,
  markNoteReclaimed,
  derivePositions,
} from "@/lib/clamm/store";
import type { TrackedNote } from "@/lib/clamm/noteStatus";

const POOL = "0xpool";

function makeNote(id: string, overrides: Partial<TrackedNote> = {}): TrackedNote {
  return {
    id,
    kind: "swap",
    senderHex: "0xsender",
    bytesHex: "00",
    serial: ["1", "2", "3", "4"],
    deadline: 100,
    createdAtBlock: 10,
    submittedAt: Date.now(),
    summary: "test",
    ...overrides,
  };
}

// Node >= 22 injects an experimental `localStorage` stub that lacks the full
// Storage interface and shadows jsdom's implementation under vitest. Install a
// functional in-memory Storage so the store code paths are actually exercised.
function memoryStorage(): Storage {
  let data = new Map<string, string>();
  return {
    get length() {
      return data.size;
    },
    clear: () => {
      data = new Map();
    },
    getItem: (key: string) => data.get(key) ?? null,
    key: (i: number) => [...data.keys()][i] ?? null,
    removeItem: (key: string) => {
      data.delete(key);
    },
    setItem: (key: string, value: string) => {
      data.set(key, value);
    },
  };
}

beforeEach(() => {
  Object.defineProperty(window, "localStorage", {
    value: memoryStorage(),
    configurable: true,
  });
});

describe("tracked notes persistence", () => {
  it("returns an empty list when nothing is stored", () => {
    expect(loadTrackedNotes(POOL)).toEqual([]);
  });

  it("round-trips notes", () => {
    const notes = [makeNote("0xa"), makeNote("0xb")];
    saveTrackedNotes(POOL, notes);
    expect(loadTrackedNotes(POOL)).toEqual(notes);
  });

  it("addTrackedNote prepends and de-duplicates by id", () => {
    addTrackedNote(POOL, makeNote("0xa", { summary: "first" }));
    addTrackedNote(POOL, makeNote("0xb"));
    const notes = addTrackedNote(POOL, makeNote("0xa", { summary: "updated" }));
    expect(notes.map((n) => n.id)).toEqual(["0xa", "0xb"]);
    expect(notes[0].summary).toBe("updated");
  });

  it("is namespaced per pool", () => {
    addTrackedNote(POOL, makeNote("0xa"));
    expect(loadTrackedNotes("0xother")).toEqual([]);
  });

  it("survives corrupted storage", () => {
    window.localStorage.setItem(`clamm:${POOL}:notes`, "not-json{");
    expect(loadTrackedNotes(POOL)).toEqual([]);
    window.localStorage.setItem(`clamm:${POOL}:notes`, JSON.stringify({ nope: true }));
    expect(loadTrackedNotes(POOL)).toEqual([]);
  });
});

describe("wallet id persistence", () => {
  it("round-trips the wallet id", () => {
    expect(loadWalletId(POOL)).toBeNull();
    saveWalletId(POOL, "0xwallet");
    expect(loadWalletId(POOL)).toBe("0xwallet");
  });
});

describe("reclaimed note ids", () => {
  it("accumulates without duplicates", () => {
    expect(loadReclaimedNoteIds(POOL)).toEqual([]);
    markNoteReclaimed(POOL, "0xa");
    markNoteReclaimed(POOL, "0xa");
    const ids = markNoteReclaimed(POOL, "0xb");
    expect(ids).toEqual(["0xa", "0xb"]);
    expect(loadReclaimedNoteIds(POOL)).toEqual(["0xa", "0xb"]);
  });
});

describe("derivePositions", () => {
  const parse = (note: TrackedNote) => {
    if (note.kind !== "mint" && note.kind !== "burn") return null;
    const [lower, upper, liq] = note.summary.split("|");
    return {
      lower: Number(lower),
      upper: Number(upper),
      liquidity: BigInt(liq),
      isBurn: note.kind === "burn",
    };
  };

  it("sums mints per range and subtracts burns (oldest first)", () => {
    // Stored newest-first: burn 400, mint 1000 (same range), mint 500 (other range).
    const notes = [
      makeNote("0xc", { kind: "burn", summary: "-120|120|400" }),
      makeNote("0xb", { kind: "mint", summary: "-240|-120|500" }),
      makeNote("0xa", { kind: "mint", summary: "-120|120|1000" }),
    ];
    expect(derivePositions(notes, parse)).toEqual([
      { tickLower: -240, tickUpper: -120, liquidity: "500" },
      { tickLower: -120, tickUpper: 120, liquidity: "600" },
    ]);
  });

  it("clamps over-burned positions to zero and ignores non-position notes", () => {
    const notes = [
      makeNote("0xswap", { kind: "swap" }),
      makeNote("0xb", { kind: "burn", summary: "-120|120|2000" }),
      makeNote("0xa", { kind: "mint", summary: "-120|120|1000" }),
    ];
    expect(derivePositions(notes, parse)).toEqual([
      { tickLower: -120, tickUpper: 120, liquidity: "0" },
    ]);
  });
});
