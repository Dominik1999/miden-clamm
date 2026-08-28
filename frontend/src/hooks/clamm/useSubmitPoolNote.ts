import { useCallback, useState } from "react";
import { useMiden, useMidenClient } from "@miden-sdk/react";
import {
  AccountId,
  Note,
  NoteArray,
  TransactionRequestBuilder,
} from "@miden-sdk/miden-sdk";
import { CLAMM_SCRIPT_URLS } from "@/config";
import type { ClammDeployment } from "@/lib/clamm/deployment";
import { accountTargetTag } from "@/lib/clamm/felts";
import {
  swapStorage,
  mintStorage,
  burnStorage,
  collectStorage,
  networkTargetWord,
} from "@/lib/clamm/encoding";
import {
  serializeClammNote,
  randomSerial,
  bytesToHex,
  type ClammNoteAsset,
} from "@/lib/clamm/noteBytes";
import type { ClammNoteKind, TrackedNote } from "@/lib/clamm/noteStatus";
import { addTrackedNote } from "@/lib/clamm/store";

export type SubmitStage = "idle" | "building" | "submitting" | "complete";

export interface SwapParams {
  direction: 0 | 1;
  amountIn: bigint;
  minOut: bigint;
  deadline: number;
}

export interface MintParams {
  tickLower: number;
  tickUpper: number;
  liquidity: bigint;
  amount0Max: bigint;
  amount1Max: bigint;
  deadline: number;
}

export interface BurnParams {
  tickLower: number;
  tickUpper: number;
  liquidity: bigint;
}

export interface CollectParams {
  tickLower: number;
  tickUpper: number;
}

export interface SubmitPoolNoteResult {
  stage: SubmitStage;
  error: string | null;
  isLoading: boolean;
  reset: () => void;
  submitSwap: (params: SwapParams) => Promise<TrackedNote | null>;
  submitMint: (params: MintParams) => Promise<TrackedNote | null>;
  submitBurn: (params: BurnParams) => Promise<TrackedNote | null>;
  submitCollect: (params: CollectParams) => Promise<TrackedNote | null>;
}

const scriptCache = new Map<string, Uint8Array>();

async function fetchScriptBytes(kind: ClammNoteKind): Promise<Uint8Array> {
  const url = CLAMM_SCRIPT_URLS[kind];
  const cached = scriptCache.get(url);
  if (cached) return cached;
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`failed to load ${kind} note script (${res.status}) — run the exporter`);
  }
  const bytes = new Uint8Array(await res.arrayBuffer());
  scriptCache.set(url, bytes);
  return bytes;
}

/**
 * Builds and publishes CLAMM pool notes, mirroring validate_local_masm's
 * `build_amm_note` + publish flow:
 *
 * 1. Serialize the full note in TS (public note, pool-targeted
 *    `NoteTag::with_account_target(pool)` tag — required for ntx-builder
 *    discovery — note storage per DESIGN Part 2, scheme-2
 *    NetworkAccountTarget attachment targeting the pool) and load it via
 *    `Note.deserialize` —
 *    the JS `Note` constructor cannot carry attachments on the 0.15 surface.
 * 2. Publish it as the wallet's own output note and submit through the
 *    configured (remote) prover.
 * 3. Record it in the tracked-note store for the lifecycle tracker.
 */
export function useSubmitPoolNote(
  deployment: ClammDeployment | null,
  walletId: string | null,
): SubmitPoolNoteResult {
  const { isReady, runExclusive, prover } = useMiden();
  const client = useMidenClient();
  const [stage, setStage] = useState<SubmitStage>("idle");
  const [error, setError] = useState<string | null>(null);

  const submit = useCallback(
    async (
      kind: ClammNoteKind,
      storage: bigint[],
      assets: ClammNoteAsset[],
      meta: Pick<
        TrackedNote,
        | "deadline"
        | "direction"
        | "inputFaucetHex"
        | "outputFaucetHex"
        | "summary"
        | "tickLower"
        | "tickUpper"
        | "liquidity"
      >,
    ): Promise<TrackedNote | null> => {
      if (!isReady || !deployment || !walletId) {
        setError("Wallet not ready");
        return null;
      }
      setError(null);
      try {
        return await runExclusive(async () => {
          setStage("building");
          const scriptBytes = await fetchScriptBytes(kind);
          const serial = randomSerial();
          const bytes = serializeClammNote({
            senderHex: walletId,
            // Tag routing: the testnet ntx-builder discovers network notes by
            // `NoteTag::with_account_target(POOL)`. A sender-derived tag
            // leaves the note silently orphaned.
            tag: accountTargetTag(deployment.pool.id),
            assets,
            scriptBytes,
            storage,
            serial,
            attachmentWord: networkTargetWord(deployment.pool.id),
          });
          // Validate + reconstruct through the WASM deserializer; this also
          // recomputes the note id from the actual content.
          const note = Note.deserialize(bytes);
          const noteId = note.id().toString();

          setStage("submitting");
          const currentBlock = await client.getSyncHeight();
          const request = new TransactionRequestBuilder()
            .withOwnOutputNotes(new NoteArray([note]))
            .build();
          if (prover) {
            await client.submitNewTransactionWithProver(
              AccountId.fromHex(walletId),
              request,
              prover,
            );
          } else {
            await client.submitNewTransaction(AccountId.fromHex(walletId), request);
          }
          await client.syncState();

          const tracked: TrackedNote = {
            id: noteId,
            kind,
            senderHex: walletId,
            bytesHex: bytesToHex(bytes),
            serial: serial.map((s) => s.toString()) as TrackedNote["serial"],
            createdAtBlock: currentBlock,
            submittedAt: Date.now(),
            ...meta,
          };
          addTrackedNote(deployment.pool.id, tracked);
          setStage("complete");
          return tracked;
        });
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
        setStage("idle");
        return null;
      }
    },
    [isReady, deployment, walletId, client, runExclusive, prover],
  );

  const submitSwap = useCallback(
    (params: SwapParams) => {
      if (!deployment || !walletId) return Promise.resolve(null);
      const inToken = params.direction === 0 ? deployment.token0 : deployment.token1;
      const outToken = params.direction === 0 ? deployment.token1 : deployment.token0;
      return submit(
        "swap",
        swapStorage({
          poolHex: deployment.pool.id,
          direction: params.direction,
          minOut: params.minOut,
          recipientHex: walletId,
          deadline: params.deadline,
        }),
        [{ faucetHex: inToken.id, amount: params.amountIn }],
        {
          deadline: params.deadline,
          direction: params.direction,
          inputFaucetHex: inToken.id,
          outputFaucetHex: outToken.id,
          summary: `Swap ${inToken.symbol} -> ${outToken.symbol}, in ${params.amountIn}, min out ${params.minOut}`,
        },
      );
    },
    [deployment, walletId, submit],
  );

  const submitMint = useCallback(
    (params: MintParams) => {
      if (!deployment) return Promise.resolve(null);
      const assets: ClammNoteAsset[] = [];
      if (params.amount0Max > 0n) {
        assets.push({ faucetHex: deployment.token0.id, amount: params.amount0Max });
      }
      if (params.amount1Max > 0n) {
        assets.push({ faucetHex: deployment.token1.id, amount: params.amount1Max });
      }
      return submit(
        "mint",
        mintStorage({
          poolHex: deployment.pool.id,
          tickLower: params.tickLower,
          tickUpper: params.tickUpper,
          liquidity: params.liquidity,
          deadline: params.deadline,
        }),
        assets,
        {
          deadline: params.deadline,
          tickLower: params.tickLower,
          tickUpper: params.tickUpper,
          liquidity: params.liquidity.toString(),
          summary: `Mint [${params.tickLower}, ${params.tickUpper}] L=${params.liquidity}`,
        },
      );
    },
    [deployment, submit],
  );

  const submitBurn = useCallback(
    (params: BurnParams) => {
      if (!deployment) return Promise.resolve(null);
      return submit(
        "burn",
        burnStorage({
          poolHex: deployment.pool.id,
          tickLower: params.tickLower,
          tickUpper: params.tickUpper,
          liquidity: params.liquidity,
        }),
        [],
        {
          deadline: 0,
          tickLower: params.tickLower,
          tickUpper: params.tickUpper,
          liquidity: params.liquidity.toString(),
          summary: `Burn [${params.tickLower}, ${params.tickUpper}] L=${params.liquidity}`,
        },
      );
    },
    [deployment, submit],
  );

  const submitCollect = useCallback(
    (params: CollectParams) => {
      if (!deployment) return Promise.resolve(null);
      return submit(
        "collect",
        collectStorage({
          poolHex: deployment.pool.id,
          tickLower: params.tickLower,
          tickUpper: params.tickUpper,
        }),
        [],
        {
          deadline: 0,
          summary: `Collect [${params.tickLower}, ${params.tickUpper}]`,
        },
      );
    },
    [deployment, submit],
  );

  const reset = useCallback(() => {
    setStage("idle");
    setError(null);
  }, []);

  return {
    stage,
    error,
    isLoading: stage === "building" || stage === "submitting",
    reset,
    submitSwap,
    submitMint,
    submitBurn,
    submitCollect,
  };
}
