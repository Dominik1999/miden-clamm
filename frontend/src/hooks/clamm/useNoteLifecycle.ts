import { useCallback, useEffect, useRef, useState } from "react";
import { useMiden, useMidenClient } from "@miden-sdk/react";
import {
  AccountId,
  Felt,
  FeltArray,
  Note,
  NoteFilter,
  NoteFilterTypes,
  NoteRecipient,
  NoteScript,
  NoteStorage,
  Poseidon2,
} from "@miden-sdk/miden-sdk";
import { CLAMM_NOTE_POLL_MS } from "@/config";
import type { ClammDeployment } from "@/lib/clamm/deployment";
import { p2idStorage } from "@/lib/clamm/encoding";
import { hexToBytes } from "@/lib/clamm/noteBytes";
import {
  deriveNoteStatus,
  type NoteStatus,
  type TrackedNote,
} from "@/lib/clamm/noteStatus";
import {
  loadTrackedNotes,
  loadReclaimedNoteIds,
  markNoteReclaimed,
} from "@/lib/clamm/store";

export interface TrackedNoteWithStatus extends TrackedNote {
  status: NoteStatus;
}

export interface ActivityItem {
  /** Note id of the incoming consumable note. */
  id: string;
  /** Fungible assets carried by the note. */
  assets: { faucetHex: string; amount: bigint }[];
  /** Tracked-note id this P2ID derives from (when matched), plus the salt. */
  sourceNoteId?: string;
  salt?: number;
}

export interface NoteLifecycleResult {
  notes: TrackedNoteWithStatus[];
  activity: ActivityItem[];
  currentBlock: number | null;
  isBusy: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  /** Consumes an expired note back into the wallet (Path B reclaim). */
  reclaim: (note: TrackedNote) => Promise<void>;
  /** Consumes an incoming P2ID note into the wallet. */
  claim: (item: ActivityItem) => Promise<void>;
}

/**
 * Computes the expected P2ID recipient digest for a pool-emitted output note:
 * serial = Poseidon2(input_serial || salt), recipient = (serial, P2ID script,
 * [target_suffix, target_prefix]) — mirroring `expected_p2id_serial` and
 * `P2idNoteStorage::into_recipient` from the Rust integration testbed.
 */
function expectedP2idDigest(note: TrackedNote, salt: number, walletId: string): string {
  const serialFelts = note.serial.map((s) => new Felt(BigInt(s)));
  const serial = Poseidon2.hashElements(
    new FeltArray([...serialFelts, new Felt(BigInt(salt))]),
  );
  const storage = new NoteStorage(
    new FeltArray(p2idStorage(walletId).map((f) => new Felt(f))),
  );
  const recipient = new NoteRecipient(serial, NoteScript.p2id(), storage);
  return recipient.digest().toHex();
}

const SALTS = [0, 1, 2, 3];

/**
 * Polls the lifecycle state of all tracked pool notes plus incoming P2ID
 * notes for the session wallet:
 *
 * - consumption: the published note's OutputNoteRecord.isConsumed()
 * - fill vs refund: which salt-derived P2ID recipient came back (salt 0 =
 *   swap output, 1 = swap refund, 2 = mint refund, 3 = collect payout)
 * - reclaimability: deadline height vs the latest synced block
 */
export function useNoteLifecycle(
  deployment: ClammDeployment | null,
  walletId: string | null,
): NoteLifecycleResult {
  const { isReady, runExclusive, prover } = useMiden();
  const client = useMidenClient();
  const [notes, setNotes] = useState<TrackedNoteWithStatus[]>([]);
  const [activity, setActivity] = useState<ActivityItem[]>([]);
  const [currentBlock, setCurrentBlock] = useState<number | null>(null);
  const [isBusy, setIsBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Digest -> {noteId, salt} lookup rebuilt on each refresh.
  const digestIndex = useRef(new Map<string, { noteId: string; salt: number }>());

  const poolId = deployment?.pool.id ?? null;

  const tagRegistered = useRef(false);

  const refresh = useCallback(async () => {
    if (!isReady || !poolId || !walletId) return;
    try {
      await runExclusive(async () => {
        // The pool emits its P2ID outputs (swap out, refunds, collect payouts)
        // with note tag 0 — subscribe once so sync picks them up (mirrors
        // validate_local_masm's `client.add_note_tag(NoteTag::from(0u32))`).
        if (!tagRegistered.current) {
          await client.addTag("0");
          tagRegistered.current = true;
        }
        await client.syncState();
        const height = await client.getSyncHeight();
        setCurrentBlock(height);

        const tracked = loadTrackedNotes(poolId);
        const reclaimed = loadReclaimedNoteIds(poolId);

        // Consumption state of our published notes.
        const outputRecords = await client.getOutputNotes(
          new NoteFilter(NoteFilterTypes.All),
        );
        const consumedIds = new Set<string>();
        for (const record of outputRecords) {
          if (record.isConsumed()) consumedIds.add(record.id().toString());
        }

        // Expected P2ID recipients for every tracked note and salt.
        digestIndex.current = new Map();
        for (const note of tracked) {
          for (const salt of SALTS) {
            digestIndex.current.set(expectedP2idDigest(note, salt, walletId), {
              noteId: note.id,
              salt,
            });
          }
        }

        // Incoming consumable notes for the wallet.
        const consumables = await client.getConsumableNotes(AccountId.fromHex(walletId));
        const items: ActivityItem[] = [];
        const matchedSaltsByNote = new Map<string, number[]>();
        for (const record of consumables) {
          const inputRecord = record.inputNoteRecord();
          const note = inputRecord.toNote();
          const digest = note.recipient().digest().toHex();
          const match = digestIndex.current.get(digest);
          if (match) {
            const salts = matchedSaltsByNote.get(match.noteId) ?? [];
            salts.push(match.salt);
            matchedSaltsByNote.set(match.noteId, salts);
          }
          items.push({
            id: note.id().toString(),
            assets: note
              .assets()
              .fungibleAssets()
              .map((a) => ({ faucetHex: a.faucetId().toString(), amount: a.amount() })),
            sourceNoteId: match?.noteId,
            salt: match?.salt,
          });
        }

        setNotes(
          tracked.map((note) => ({
            ...note,
            status: deriveNoteStatus({
              kind: note.kind,
              consumed: consumedIds.has(note.id),
              reclaimedByUser: reclaimed.includes(note.id),
              currentBlock: height,
              deadline: note.deadline,
              matchedSalts: matchedSaltsByNote.get(note.id) ?? [],
            }),
          })),
        );
        setActivity(items);
        setError(null);
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [isReady, poolId, walletId, client, runExclusive]);

  useEffect(() => {
    if (!isReady || !poolId || !walletId) return;
    let cancelled = false;
    const tick = () => {
      if (!cancelled) void refresh();
    };
    tick();
    const interval = setInterval(tick, CLAMM_NOTE_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [isReady, poolId, walletId, refresh]);

  const submitConsume = useCallback(
    async (accountHex: string, toConsume: Note[]) => {
      const request = client.newConsumeTransactionRequest(toConsume);
      if (prover) {
        await client.submitNewTransactionWithProver(
          AccountId.fromHex(accountHex),
          request,
          prover,
        );
      } else {
        await client.submitNewTransaction(AccountId.fromHex(accountHex), request);
      }
      await client.syncState();
    },
    [client, prover],
  );

  const reclaim = useCallback(
    async (note: TrackedNote) => {
      if (!isReady || !poolId || !walletId) return;
      setIsBusy(true);
      setError(null);
      try {
        await runExclusive(async () => {
          // Rebuild the full Note from the persisted bytes (Path B: the sender
          // consumes their own expired note; assets return via receive_asset).
          const rebuilt = Note.deserialize(hexToBytes(note.bytesHex));
          await submitConsume(walletId, [rebuilt]);
          markNoteReclaimed(poolId, note.id);
        });
        await refresh();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setIsBusy(false);
      }
    },
    [isReady, poolId, walletId, runExclusive, submitConsume, refresh],
  );

  const claim = useCallback(
    async (item: ActivityItem) => {
      if (!isReady || !walletId) return;
      setIsBusy(true);
      setError(null);
      try {
        await runExclusive(async () => {
          const consumables = await client.getConsumableNotes(AccountId.fromHex(walletId));
          const toConsume = consumables
            .map((c) => c.inputNoteRecord().toNote())
            .filter((n) => n.id().toString() === item.id);
          if (toConsume.length === 0) {
            throw new Error("Note is no longer consumable");
          }
          await submitConsume(walletId, toConsume);
        });
        await refresh();
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setIsBusy(false);
      }
    },
    [isReady, walletId, client, runExclusive, submitConsume, refresh],
  );

  return { notes, activity, currentBlock, isBusy, error, refresh, reclaim, claim };
}
