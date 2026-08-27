import { useEffect, useState, useCallback, useRef } from "react";
import { useMiden, useMidenClient } from "@miden-sdk/react";
import {
  TransactionRequestBuilder,
  Package,
  NoteScript,
  Note,
  NoteAssets,
  NoteMetadata,
  NoteRecipient,
  NoteStorage,
  NoteTag,
  NoteType,
  NoteArray,
  FeltArray,
  AccountId,
  AccountStorageMode,
  Felt,
  Word,
  type Account,
  type TransactionRequest,
} from "@miden-sdk/miden-sdk";
import { randomWord } from "@/lib/miden";
import {
  COUNTER_SLOT_NAME,
  EXPLORER_BASE_URL,
  INCREMENT_NOTE_PACKAGE_URL,
  NETWORK_POLL_INTERVAL_MS,
  NETWORK_POLL_TIMEOUT_MS,
} from "@/config";

// Numeric wasm `AuthScheme` discriminant for RpoFalcon512, expected by the raw
// `WebClient.newWallet(storageMode, authScheme, seed)` (it forwards the value
// straight to wasm-bindgen). The package's *public* `AuthScheme` export is the
// friendly `{ Falcon: "falcon", ECDSA: "ecdsa" }` string const — NOT this numeric
// enum — so `AuthScheme.AuthRpoFalcon512` is `undefined`, and passing it makes the
// wasm worker panic ("invalid enum value passed") with the `newWallet` promise then
// hanging. The React SDK's `useCreateWallet` hook can't stand in here for the same
// reason (its `DEFAULTS.AUTH_SCHEME` resolves through that same const). Verified
// against the wasm glue: `AuthScheme = Object.freeze({ AuthEcdsaK256Keccak: 1, AuthRpoFalcon512: 2 })`.
const AUTH_SCHEME_RPO_FALCON512 = 2;

// Fixed storage key the counter component uses for its value: Word [0,0,0,1].
const countKey = () =>
  Word.newFromFelts([new Felt(0n), new Felt(0n), new Felt(0n), new Felt(1n)]);

// Read the counter value out of an account's storage map.
function readCount(account: Account): number {
  const value = account.storage().getMapItem(COUNTER_SLOT_NAME, countKey());
  return value ? Number(value.toU64s()[0]) : 0;
}

// Resolve a counter address into an `AccountId`, accepting either the SDK's
// canonical hex id (`0x…`, what `AccountId.toString()` / the deploy tooling emit)
// or a bech32 address (`mtst1…`, what the block explorer shows) so "point at your
// own counter" works with whichever form you have.
const parseAccountId = (address: string): AccountId =>
  address.startsWith("0x")
    ? AccountId.fromHex(address)
    : AccountId.fromBech32(address);

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * v0.15 counter increment.
 *
 * The counter is a public, `NoAuth` account, so anyone can execute a transaction
 * against it. Incrementing is a two-transaction flow (mirroring the
 * project-template `increment_count` reference):
 *   1. a sender publishes the increment note (built from `increment-note.masp`),
 *   2. the counter consumes that note, which runs `increment_count` on itself.
 * Both transactions are submitted by the local in-browser client — the counter
 * needs no signature (NoAuth), and the sender is a throwaway local wallet. No
 * network-account attachment is involved on v0.15.
 */
export function useIncrementCounter(counterAddress: string) {
  const [error, setError] = useState<string | null>(null);
  // Non-null while an increment is in flight; also drives the button label.
  const [status, setStatus] = useState<string | null>(null);
  const [count, setCount] = useState<number | null>(null);

  const { runExclusive, isReady, prover } = useMiden();
  const client = useMidenClient();
  // Throwaway local sender that publishes the increment notes (created once).
  const senderRef = useRef<Account | null>(null);

  const busy = status !== null;

  // Fetch the on-chain counter value: import the public counter on first call,
  // sync, and read its storage map. All WASM calls are serialized via
  // runExclusive to avoid "recursive use of an object" errors.
  const loadCount = useCallback(async (): Promise<number | null> => {
    if (!isReady || !counterAddress) return null;
    return await runExclusive(async () => {
      const counterId = parseAccountId(counterAddress);
      if (!(await client.getAccount(counterId))) {
        await client.importAccountById(counterId);
      }
      await client.syncState();
      const account = await client.getAccount(counterId);
      if (!account) {
        setCount(null);
        setError(
          `Counter account not found on-chain (${counterAddress}). Check VITE_MIDEN_COUNTER_ADDRESS / src/config.ts and confirm the counter is deployed on the configured network.`,
        );
        return null;
      }
      const newCount = readCount(account);
      setCount(newCount);
      setError(null);
      return newCount;
    });
  }, [isReady, client, runExclusive, counterAddress]);

  useEffect(() => {
    loadCount().catch((err) => {
      setError(err instanceof Error ? err.message : String(err));
    });
  }, [loadCount]);

  const increment = useCallback(async () => {
    if (!isReady) return;
    setError(null);
    const before = count ?? 0;
    try {
      await runExclusive(async () => {
        // `AccountId` is consumed (moved) by some WASM calls — notably
        // `getConsumableNotes` (`account_id.__destroy_into_raw()`), which we call
        // in a loop — so mint a fresh id wherever the counter id is passed by
        // value. (`getAccount` borrows and is safe to reuse, but we stay uniform.)
        const freshCounterId = () => parseAccountId(counterAddress);

        // Make sure we have a local sender to publish the note from.
        if (!senderRef.current) {
          setStatus("Preparing sender...");
          // Private storage keeps the throwaway sender's account-id seed grind
          // cheap (it only needs to publish a public note; its own state never
          // needs to be on-chain). This matches the SDK's default wallet mode.
          senderRef.current = await client.newWallet(
            AccountStorageMode.private(),
            AUTH_SCHEME_RPO_FALCON512,
            undefined,
          );
          await client.syncState();
        }
        const sender = senderRef.current;

        // Build the increment note from the compiled package.
        const buf = await fetch(INCREMENT_NOTE_PACKAGE_URL).then((r) =>
          r.arrayBuffer(),
        );
        const pkg = Package.deserialize(new Uint8Array(buf));
        const noteScript = NoteScript.fromPackage(pkg);
        const recipient = new NoteRecipient(
          randomWord(),
          noteScript,
          new NoteStorage(new FeltArray()),
        );
        const metadata = new NoteMetadata(
          sender.id(),
          NoteType.Public,
          new NoteTag(0),
        );
        const note = new Note(new NoteAssets(), metadata, recipient);
        // Capture the note id BEFORE `note` is moved into the publish request:
        // wasm-bindgen takes ownership of value-class args (here `new NoteArray([note])`),
        // zeroing the JS handle, so the object can't be reused afterwards.
        const noteId = note.id().toString();

        // Submit through the remote prover so proof generation is offloaded
        // (the worker-less single-threaded client then only pays local
        // execution, not the minutes-long single-threaded proof). Falls back to
        // the client's default (local) prover if none is configured.
        const submitTx = (
          accountId: ReturnType<typeof freshCounterId>,
          request: TransactionRequest,
        ) =>
          prover
            ? client.submitNewTransactionWithProver(accountId, request, prover)
            : client.submitNewTransaction(accountId, request);

        // (1) Publish the note as the sender's own output note.
        setStatus("Publishing increment note...");
        const publishRequest = new TransactionRequestBuilder()
          .withOwnOutputNotes(new NoteArray([note]))
          .build();
        await submitTx(sender.id(), publishRequest);
        await client.syncState();

        // (2) (Re)import the counter so THIS client instance tracks its latest
        // on-chain state and registers it in the SMT forest that `apply` reads
        // (importAccountById uses overwrite=true -> re-fetch + re-register). This
        // is unconditional on purpose: gating on `getAccount` would skip the
        // re-registration when a stale record already sits in IndexedDB, and the
        // consume's apply step would then fail with "account data wasn't found".
        // Then read the current on-chain value as the baseline — the counter is
        // shared, so it may already have advanced past the displayed `count`.
        setStatus("Waiting for note to commit...");
        await client.importAccountById(freshCounterId());
        await client.syncState();
        const baseAccount = await client.getAccount(freshCounterId());
        const baseCount = baseAccount ? readCount(baseAccount) : before;

        // Wait until the counter can consume our note, then rebuild fresh `Note`
        // objects from the on-chain consumable records (the published `note`
        // handle is spent) and keep only the one we just published.
        const publishDeadline = Date.now() + NETWORK_POLL_TIMEOUT_MS;
        let toConsume: Note[] = [];
        while (toConsume.length === 0 && Date.now() < publishDeadline) {
          await sleep(NETWORK_POLL_INTERVAL_MS);
          await client.syncState();
          const consumables = await client.getConsumableNotes(freshCounterId());
          toConsume = consumables
            .map((c) => c.inputNoteRecord().toNote())
            .filter((n) => n.id().toString() === noteId);
        }
        if (toConsume.length === 0) {
          setError(
            `Increment note did not become consumable within ${Math.round(
              NETWORK_POLL_TIMEOUT_MS / 1000,
            )}s. Refresh and try again.`,
          );
          return;
        }

        // (3) Consume the note as the counter (NoAuth -> no signature needed).
        setStatus("Incrementing (consuming note)...");
        const consumeRequest = client.newConsumeTransactionRequest(toConsume);
        await submitTx(freshCounterId(), consumeRequest);

        // Wait for the on-chain count to advance past the pre-consume baseline.
        setStatus("Confirming new count...");
        const confirmDeadline = Date.now() + NETWORK_POLL_TIMEOUT_MS;
        let latest = baseCount;
        while (latest === baseCount && Date.now() < confirmDeadline) {
          await sleep(NETWORK_POLL_INTERVAL_MS);
          await client.syncState();
          const account = await client.getAccount(freshCounterId());
          if (account) latest = readCount(account);
        }
        setCount(latest);
        if (latest === baseCount) {
          setError(
            `Increment submitted, but the counter did not update within ${Math.round(
              NETWORK_POLL_TIMEOUT_MS / 1000,
            )}s. Refresh to re-check.`,
          );
        }
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setStatus(null);
    }
  }, [isReady, client, runExclusive, counterAddress, count, prover]);

  return {
    increment,
    count,
    isSubmitting: busy,
    status,
    error,
    explorerUrl: `${EXPLORER_BASE_URL}/account/${counterAddress}`,
  };
}
