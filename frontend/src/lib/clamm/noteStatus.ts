// Tracked-note types and the note lifecycle state machine.
//
// Lifecycle (DESIGN.md Part 2): a published network note is
//   pending      — committed on-chain, not yet consumed by the ntx-builder
//   filled       — consumed by the pool; the swap-output P2ID (salt 0) came back
//   refunded     — consumed by the pool via the deadline-refund path; the
//                  refund P2ID (salt 1) came back with the input asset
//   processed    — consumed by the pool (mint/burn/collect, or a swap whose
//                  result P2ID has not been matched yet)
//   reclaimable  — deadline passed and the note is still unconsumed; the
//                  sender can consume it with their own wallet (Path B)
//   reclaimed    — the sender consumed it themselves

import { P2ID_SALT } from "./encoding";

export type ClammNoteKind = "swap" | "mint" | "burn" | "collect";

export type NoteStatus =
  | "pending"
  | "filled"
  | "refunded"
  | "processed"
  | "reclaimable"
  | "reclaimed";

export interface TrackedNote {
  /** Note id hex (0x…), captured at publish time. */
  id: string;
  kind: ClammNoteKind;
  senderHex: string;
  /** Full serialized note bytes (hex, no 0x) — enables reclaim + P2ID derivation. */
  bytesHex: string;
  /** Serial number felts as decimal strings. */
  serial: [string, string, string, string];
  /** Deadline block height (swap/mint; 0 for burn/collect which have none). */
  deadline: number;
  /** Chain height when the note was submitted. */
  createdAtBlock: number;
  submittedAt: number;
  /** Swap only: 0 = zero_for_one, 1 = one_for_zero. */
  direction?: 0 | 1;
  /** Mint/burn only: position range and liquidity (decimal string, u128). */
  tickLower?: number;
  tickUpper?: number;
  liquidity?: string;
  /** Faucet hex of the asset(s) locked in the note. */
  inputFaucetHex?: string;
  /** Swap only: faucet hex of the expected output token. */
  outputFaucetHex?: string;
  /** Human summary of amounts/ticks for the UI. */
  summary: string;
}

export interface NoteStatusInputs {
  kind: ClammNoteKind;
  /** The note's nullifier has been spent on-chain. */
  consumed: boolean;
  /** The user consumed the note themselves (local reclaim tx submitted/committed). */
  reclaimedByUser: boolean;
  /** Latest known chain height (null before first sync). */
  currentBlock: number | null;
  /** Deadline height; 0/undefined means no deadline (burn/collect). */
  deadline: number;
  /** P2ID salts whose derived output notes have been observed for the sender. */
  matchedSalts: number[];
}

/** Derives the display status of a tracked note. */
export function deriveNoteStatus(inputs: NoteStatusInputs): NoteStatus {
  const { kind, consumed, reclaimedByUser, currentBlock, deadline, matchedSalts } = inputs;

  if (reclaimedByUser) return "reclaimed";

  if (consumed) {
    if (kind === "swap") {
      if (matchedSalts.includes(P2ID_SALT.swapOut)) return "filled";
      if (matchedSalts.includes(P2ID_SALT.swapRefund)) return "refunded";
      return "processed";
    }
    return "processed";
  }

  if (deadline > 0 && currentBlock !== null && currentBlock >= deadline) {
    return "reclaimable";
  }
  return "pending";
}

/** Human-readable labels for statuses. */
export const NOTE_STATUS_LABELS: Record<NoteStatus, string> = {
  pending: "Pending",
  filled: "Filled",
  refunded: "Refunded",
  processed: "Processed",
  reclaimable: "Reclaimable",
  reclaimed: "Reclaimed",
};
