import { renderHook, act, waitFor } from "@testing-library/react";
import { vi, describe, it, expect, beforeEach } from "vitest";

const mockFetch = vi.fn(async () => ({
  arrayBuffer: async () => new ArrayBuffer(0),
}));
vi.stubGlobal("fetch", mockFetch);

vi.mock("@miden-sdk/react", () => import("@/__tests__/mocks/miden-sdk-react"));

// The increment flow builds a note (Package/NoteScript/Note/... constructors),
// creates a local sender, publishes it, and consumes it as the counter. The SDK
// value classes are stubbed by a Proxy; the client methods are mocked below so
// we can assert the two-transaction (publish + consume) sequence and the count.
vi.mock("@miden-sdk/miden-sdk", async () => {
  const stub = (): object =>
    new Proxy(function noop() {}, {
      get: (_t, prop) => {
        if (prop === "toU64s") return () => [0n, 0n, 0n, 0n];
        // Stable id so the published note matches the consumable record the
        // client returns (the hook filters consumables by note-id string).
        if (prop === "toString") return () => "STUB_NOTE_ID";
        if (typeof prop === "symbol") return undefined;
        return stub();
      },
      apply: () => stub(),
      construct: () => stub(),
    });
  const exports: Record<string, unknown> = {};
  for (const k of [
    "TransactionRequestBuilder",
    "Package",
    "NoteScript",
    "Note",
    "NoteAssets",
    "NoteMetadata",
    "NoteRecipient",
    "NoteStorage",
    "NoteTag",
    "NoteType",
    "NoteArray",
    "FeltArray",
    "AccountId",
    "AccountStorageMode",
    "AuthScheme",
    "Felt",
    "Word",
  ]) {
    exports[k] = stub();
  }
  return exports;
});

vi.mock("@/lib/miden", () => ({ randomWord: () => ({}) }));

import { useMiden, useMidenClient } from "@miden-sdk/react";
import { useIncrementCounter } from "../useIncrementCounter";

const COUNTER_ADDRESS = "0x4dcaee76ffebfc511e06582702289d";

// A stub Account whose stored count reads from a mutable holder.
const accountWithCount = (holder: { n: number }) =>
  ({
    storage: () => ({
      getMapItem: () => ({ toU64s: () => [BigInt(holder.n), 0n, 0n, 0n] }),
    }),
    id: () => ({ toString: () => "0xsender" }),
  }) as never;

const mockImportAccountById = vi.fn(async () => undefined);
const mockSyncState = vi.fn(async () => undefined);
const mockNewWallet = vi.fn();
const mockSubmitNewTransaction = vi.fn(async () => ({ toHex: () => "0xtx" }));
// A consumable record whose `inputNoteRecord().toNote().id().toString()` matches
// the published note's id ("STUB_NOTE_ID"), so the hook's id filter keeps it.
const fakeConsumableRecord = {
  inputNoteRecord: () => ({
    toNote: () => ({ id: () => ({ toString: () => "STUB_NOTE_ID" }) }),
  }),
};
const mockGetConsumableNotes = vi.fn(async () => [fakeConsumableRecord]);
const mockNewConsumeTransactionRequest = vi.fn(() => ({}));

describe("useIncrementCounter", () => {
  beforeEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();

    vi.mocked(useMiden).mockReturnValue({
      client: null,
      isReady: true,
      isInitializing: false,
      error: null,
      sync: vi.fn(),
      runExclusive: <T,>(fn: () => Promise<T>) => fn(),
      prover: null,
      signerAccountId: null,
      signerConnected: null,
    });
  });

  it("loads the counter value from on-chain storage (read path)", async () => {
    const holder = { n: 7 };
    vi.mocked(useMidenClient).mockReturnValue({
      getAccount: vi.fn(async () => accountWithCount(holder)),
      importAccountById: mockImportAccountById,
      syncState: mockSyncState,
    } as unknown as ReturnType<typeof useMidenClient>);

    const { result } = renderHook(() => useIncrementCounter(COUNTER_ADDRESS));
    await waitFor(() => expect(result.current.count).toBe(7));
    expect(result.current.error).toBeNull();
  });

  it("surfaces an error when the counter account is unreachable on-chain", async () => {
    vi.mocked(useMidenClient).mockReturnValue({
      getAccount: vi.fn(async () => null),
      importAccountById: mockImportAccountById,
      syncState: mockSyncState,
    } as unknown as ReturnType<typeof useMidenClient>);

    const { result } = renderHook(() => useIncrementCounter(COUNTER_ADDRESS));
    await waitFor(() =>
      expect(result.current.error).toMatch(/counter account not found/i),
    );
    expect(result.current.count).toBeNull();
  });

  it("increments by publishing the note then consuming it as the counter", async () => {
    const holder = { n: 3 };
    // The counter's count advances only after a consume tx is submitted against it.
    mockSubmitNewTransaction.mockImplementation(async () => {
      holder.n += 1; // simulate the on-chain effect on consume/publish
      return { toHex: () => "0xtx" } as never;
    });
    // Reset holder bump: only the consume (2nd submit) should reflect the count
    // change we assert on; both publish+consume call submit, which is fine here.
    vi.mocked(useMidenClient).mockReturnValue({
      getAccount: vi.fn(async () => accountWithCount(holder)),
      importAccountById: mockImportAccountById,
      syncState: mockSyncState,
      newWallet: mockNewWallet.mockResolvedValue({
        id: () => ({ toString: () => "0xsender" }),
      }),
      submitNewTransaction: mockSubmitNewTransaction,
      getConsumableNotes: mockGetConsumableNotes,
      newConsumeTransactionRequest: mockNewConsumeTransactionRequest,
    } as unknown as ReturnType<typeof useMidenClient>);

    const { result } = renderHook(() => useIncrementCounter(COUNTER_ADDRESS));
    await waitFor(() => expect(result.current.count).toBe(3));

    vi.useFakeTimers();
    try {
      let p: Promise<void> | undefined;
      await act(async () => {
        p = result.current.increment();
        await Promise.resolve();
      });
      // Fast-forward through the publish-commit and confirm wait loops.
      for (let i = 0; i < 40 && result.current.isSubmitting; i++) {
        await act(async () => {
          await vi.advanceTimersByTimeAsync(2_600);
        });
      }
      await act(async () => {
        await p;
      });
    } finally {
      vi.useRealTimers();
    }

    // A local sender was created, the note was published and then consumed:
    expect(mockNewWallet).toHaveBeenCalledTimes(1);
    expect(mockNewConsumeTransactionRequest).toHaveBeenCalledTimes(1);
    // submitNewTransaction is called at least twice (publish + consume).
    expect(mockSubmitNewTransaction.mock.calls.length).toBeGreaterThanOrEqual(2);
    // The observed count advanced past the starting value.
    expect(result.current.count).toBeGreaterThan(3);
    expect(result.current.error).toBeNull();
  });
});
