// localStorage-backed persistence for the CLAMM UI: the session wallet id,
// tracked pool notes, and reclaim/position bookkeeping. All storage access is
// wrapped in try/catch — private windows or cleared site data must degrade to
// empty state, never crash the app.

import type { TrackedNote } from "./noteStatus";

export interface TrackedPosition {
  tickLower: number;
  tickUpper: number;
  /** Total liquidity minted through this UI (decimal string, u128). */
  liquidity: string;
}

function notesKey(poolId: string): string {
  return `clamm:${poolId}:notes`;
}

function walletKey(poolId: string): string {
  return `clamm:${poolId}:wallet`;
}

function reclaimedKey(poolId: string): string {
  return `clamm:${poolId}:reclaimed`;
}

// Resolve the browser's localStorage explicitly through `window` — a bare
// `localStorage` global can resolve to Node's experimental stub under test
// runners, which lacks the full Storage interface.
function storage(): Storage | null {
  try {
    return typeof window !== "undefined" ? window.localStorage : null;
  } catch {
    return null;
  }
}

function readJson<T>(key: string, fallback: T): T {
  try {
    const raw = storage()?.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

function writeJson(key: string, value: unknown): void {
  try {
    storage()?.setItem(key, JSON.stringify(value));
  } catch {
    // Storage unavailable — tracked state is a per-browser convenience only.
  }
}

export function loadTrackedNotes(poolId: string): TrackedNote[] {
  const notes = readJson<TrackedNote[]>(notesKey(poolId), []);
  return Array.isArray(notes) ? notes : [];
}

export function saveTrackedNotes(poolId: string, notes: TrackedNote[]): void {
  writeJson(notesKey(poolId), notes);
}

export function addTrackedNote(poolId: string, note: TrackedNote): TrackedNote[] {
  const notes = loadTrackedNotes(poolId).filter((n) => n.id !== note.id);
  notes.unshift(note);
  saveTrackedNotes(poolId, notes);
  return notes;
}

export function loadWalletId(poolId: string): string | null {
  try {
    return storage()?.getItem(walletKey(poolId)) ?? null;
  } catch {
    return null;
  }
}

export function saveWalletId(poolId: string, walletId: string): void {
  try {
    storage()?.setItem(walletKey(poolId), walletId);
  } catch {
    // ignore
  }
}

export function loadReclaimedNoteIds(poolId: string): string[] {
  const ids = readJson<string[]>(reclaimedKey(poolId), []);
  return Array.isArray(ids) ? ids : [];
}

export function markNoteReclaimed(poolId: string, noteId: string): string[] {
  const ids = loadReclaimedNoteIds(poolId);
  if (!ids.includes(noteId)) ids.push(noteId);
  writeJson(reclaimedKey(poolId), ids);
  return ids;
}

/**
 * Derives the position list from tracked mint/burn notes: sums minted
 * liquidity per (tickLower, tickUpper) range. Only positions minted through
 * this browser are listed (the on-chain position map is keyed by a Poseidon2
 * hash, so it cannot be enumerated without knowing the ranges).
 */
export function derivePositions(
  notes: TrackedNote[],
  parse: (note: TrackedNote) => { lower: number; upper: number; liquidity: bigint; isBurn: boolean } | null,
): TrackedPosition[] {
  const byRange = new Map<string, bigint>();
  // Notes are stored newest-first; apply oldest-first.
  for (const note of [...notes].reverse()) {
    const parsed = parse(note);
    if (!parsed) continue;
    const key = `${parsed.lower}:${parsed.upper}`;
    const current = byRange.get(key) ?? 0n;
    const next = parsed.isBurn ? current - parsed.liquidity : current + parsed.liquidity;
    byRange.set(key, next < 0n ? 0n : next);
  }
  const positions: TrackedPosition[] = [];
  for (const [key, liquidity] of byRange) {
    const [lower, upper] = key.split(":").map(Number);
    positions.push({ tickLower: lower, tickUpper: upper, liquidity: liquidity.toString() });
  }
  positions.sort((a, b) => a.tickLower - b.tickLower || a.tickUpper - b.tickUpper);
  return positions;
}
