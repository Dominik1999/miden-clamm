import { useCallback, useEffect, useState } from "react";
import { useMiden, useMidenClient } from "@miden-sdk/react";
import { AccountId, type Account } from "@miden-sdk/miden-sdk";
import { CLAMM_POOL_POLL_MS } from "@/config";
import { POOL_SLOTS, type ClammDeployment } from "@/lib/clamm/deployment";
import { limbsToU128 } from "@/lib/clamm/felts";
import { feltToTick } from "@/lib/clamm/encoding";

export interface PoolState {
  sqrtPriceX96: bigint;
  tick: number;
  liquidity: bigint;
  feePips: number;
  tickSpacing: number;
  /** Latest locally synced block height. */
  blockHeight: number;
}

export interface PoolStateResult {
  poolState: PoolState | null;
  isLoading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
}

// `AccountStorage.getItem` returns a raw WASM `Word` on the low-level surface,
// but the React SDK wraps storage reads in a `StorageResult` helper (methods:
// `word()`, `toFelts()`, `toHex()`, …). Normalize both shapes to u64 limbs.
type WordLike = { toU64s(): BigUint64Array };

function itemU64s(item: unknown): bigint[] {
  const value = item as Partial<WordLike> &
    Partial<{ word(): WordLike }> &
    Partial<{ toFelts(): { asInt(): bigint }[] }>;
  if (typeof value.toU64s === "function") {
    return Array.from(value.toU64s());
  }
  if (typeof value.word === "function") {
    return Array.from(value.word().toU64s());
  }
  if (typeof value.toFelts === "function") {
    return value.toFelts().map((felt) => felt.asInt());
  }
  throw new Error("unsupported storage item shape (no toU64s/word/toFelts)");
}

function readPoolState(account: Account, blockHeight: number): PoolState {
  const storage = account.storage();
  const sqrtPrice = storage.getItem(POOL_SLOTS.sqrtPrice);
  const poolStateWord = storage.getItem(POOL_SLOTS.poolState);
  const liquidity = storage.getItem(POOL_SLOTS.liquidity);
  const poolParams = storage.getItem(POOL_SLOTS.poolParams);
  if (!sqrtPrice || !poolStateWord || !liquidity || !poolParams) {
    throw new Error("pool account is missing CLAMM storage slots");
  }
  const params = itemU64s(poolParams);
  return {
    sqrtPriceX96: limbsToU128(itemU64s(sqrtPrice)),
    tick: feltToTick(itemU64s(poolStateWord)[0]),
    liquidity: limbsToU128(itemU64s(liquidity)),
    feePips: Number(params[0]),
    tickSpacing: Number(params[1]),
    blockHeight,
  };
}

/**
 * Polls the pool account's public storage. The pool is updated externally by
 * network transactions, so every refresh re-imports the account
 * (`importAccountById` is overwrite=true) before reading the slots.
 */
export function usePoolState(deployment: ClammDeployment | null): PoolStateResult {
  const { isReady, runExclusive } = useMiden();
  const client = useMidenClient();
  const [poolState, setPoolState] = useState<PoolState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);

  const poolId = deployment?.pool.id ?? null;

  const refresh = useCallback(async () => {
    if (!isReady || !poolId) return;
    try {
      await runExclusive(async () => {
        // AccountId objects are consumed by some WASM calls — mint fresh ones.
        await client.importAccountById(AccountId.fromHex(poolId));
        await client.syncState();
        const account = await client.getAccount(AccountId.fromHex(poolId));
        if (!account) {
          throw new Error(`pool account ${poolId} not found on-chain`);
        }
        const height = await client.getSyncHeight();
        setPoolState(readPoolState(account, height));
        setError(null);
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsLoading(false);
    }
  }, [isReady, client, runExclusive, poolId]);

  useEffect(() => {
    if (!isReady || !poolId) return;
    let cancelled = false;
    const tick = () => {
      if (!cancelled) void refresh();
    };
    tick();
    const interval = setInterval(tick, CLAMM_POOL_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [isReady, poolId, refresh]);

  return { poolState, isLoading, error, refresh };
}
