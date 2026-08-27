import { useCallback, useEffect, useRef, useState } from "react";
import { useMiden, useMidenClient } from "@miden-sdk/react";
import {
  AccountId,
  AccountStorageMode,
  AuthSecretKey,
  NoteType,
  type Note,
} from "@miden-sdk/miden-sdk";
import { CLAMM_FAUCET_AMOUNT, NETWORK_POLL_INTERVAL_MS, NETWORK_POLL_TIMEOUT_MS } from "@/config";
import type { ClammDeployment, ClammTokenInfo } from "@/lib/clamm/deployment";
import { loadWalletId, saveWalletId } from "@/lib/clamm/store";
import { hexToBytes } from "@/lib/clamm/noteBytes";

// Numeric wasm AuthScheme discriminant for RpoFalcon512 (see useIncrementCounter).
const AUTH_SCHEME_RPO_FALCON512 = 2;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export interface ClammWalletResult {
  /** Hex id of the local session wallet (null until created). */
  walletId: string | null;
  balances: { token0: bigint; token1: bigint };
  isBusy: boolean;
  status: string | null;
  error: string | null;
  /** Creates (or restores) the session wallet. */
  ensureWallet: () => Promise<void>;
  /** Mints test tokens from the dev faucet to the session wallet and consumes them. */
  fund: (token: "token0" | "token1") => Promise<void>;
  refreshBalances: () => Promise<void>;
}

/**
 * Local session wallet for the CLAMM UI. Mirrors the validated Rust flow's
 * user wallets: transactions are signed by the in-browser client (local
 * keystore), not a wallet extension. The wallet id is persisted per pool in
 * localStorage; the account itself lives in IndexedDB.
 *
 * Funding uses the DEV-ONLY faucet secret keys from deployment.json: the
 * browser imports the on-chain faucet account, adds its key to the local
 * keystore, submits the mint as the faucet, then consumes the P2ID as the
 * wallet — the same two-step flow validate_local_masm runs in Rust.
 */
export function useClammWallet(deployment: ClammDeployment | null): ClammWalletResult {
  const { isReady, runExclusive, prover } = useMiden();
  const client = useMidenClient();
  const [walletId, setWalletId] = useState<string | null>(null);
  const [balances, setBalances] = useState({ token0: 0n, token1: 0n });
  const [status, setStatus] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const importedFaucets = useRef(new Set<string>());

  const poolId = deployment?.pool.id ?? null;

  const submitTx = useCallback(
    async (accountId: AccountId, request: Parameters<typeof client.submitNewTransaction>[1]) =>
      prover
        ? client.submitNewTransactionWithProver(accountId, request, prover)
        : client.submitNewTransaction(accountId, request),
    [client, prover],
  );

  const readBalances = useCallback(
    async (idHex: string) => {
      if (!deployment) return { token0: 0n, token1: 0n };
      const vault = await client.getAccountVault(AccountId.fromHex(idHex));
      return {
        token0: vault.getBalance(AccountId.fromHex(deployment.token0.id)),
        token1: vault.getBalance(AccountId.fromHex(deployment.token1.id)),
      };
    },
    [client, deployment],
  );

  const ensureWallet = useCallback(async () => {
    if (!isReady || !poolId) return;
    setError(null);
    try {
      await runExclusive(async () => {
        const stored = loadWalletId(poolId);
        if (stored) {
          const existing = await client.getAccount(AccountId.fromHex(stored));
          if (existing) {
            setWalletId(stored);
            setBalances(await readBalances(stored));
            return;
          }
        }
        setStatus("Creating session wallet...");
        const account = await client.newWallet(
          AccountStorageMode.private(),
          AUTH_SCHEME_RPO_FALCON512,
          undefined,
        );
        const idHex = account.id().toString();
        saveWalletId(poolId, idHex);
        setWalletId(idHex);
        setBalances({ token0: 0n, token1: 0n });
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setStatus(null);
    }
  }, [isReady, poolId, client, runExclusive, readBalances]);

  // Restore/create the wallet as soon as the client and deployment are ready.
  useEffect(() => {
    void ensureWallet();
  }, [ensureWallet]);

  const refreshBalances = useCallback(async () => {
    if (!isReady || !walletId) return;
    try {
      await runExclusive(async () => {
        await client.syncState();
        setBalances(await readBalances(walletId));
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, [isReady, walletId, client, runExclusive, readBalances]);

  const fund = useCallback(
    async (token: "token0" | "token1") => {
      if (!isReady || !walletId || !deployment) return;
      const info: ClammTokenInfo = deployment[token];
      if (!info.devSecretKeyHex) {
        setError(`No dev faucet key for ${info.symbol} in deployment.json`);
        return;
      }
      setError(null);
      try {
        await runExclusive(async () => {
          // 1. Track the on-chain faucet and register its dev key.
          if (!importedFaucets.current.has(info.id)) {
            setStatus(`Importing ${info.symbol} faucet...`);
            if (!(await client.getAccount(AccountId.fromHex(info.id)))) {
              await client.importAccountById(AccountId.fromHex(info.id));
            }
            await client.addAccountSecretKeyToWebStore(
              AccountId.fromHex(info.id),
              AuthSecretKey.deserialize(hexToBytes(info.devSecretKeyHex!)),
            );
            importedFaucets.current.add(info.id);
          }

          // 2. Mint (executes on the faucet).
          setStatus(`Minting ${info.symbol}...`);
          const mintRequest = await client.newMintTransactionRequest(
            AccountId.fromHex(walletId),
            AccountId.fromHex(info.id),
            NoteType.Public,
            CLAMM_FAUCET_AMOUNT,
          );
          await submitTx(AccountId.fromHex(info.id), mintRequest);
          await client.syncState();

          // 3. Wait for the mint note, then consume it as the wallet.
          setStatus(`Waiting for ${info.symbol} mint note...`);
          const deadline = Date.now() + NETWORK_POLL_TIMEOUT_MS;
          let toConsume: Note[] = [];
          while (toConsume.length === 0 && Date.now() < deadline) {
            await sleep(NETWORK_POLL_INTERVAL_MS);
            await client.syncState();
            const consumables = await client.getConsumableNotes(AccountId.fromHex(walletId));
            toConsume = consumables
              .map((c) => c.inputNoteRecord().toNote())
              .filter((n) => {
                const assets = n.assets().fungibleAssets();
                return assets.some((a) => a.faucetId().toString() === info.id);
              });
          }
          if (toConsume.length === 0) {
            throw new Error(`${info.symbol} mint note did not arrive in time`);
          }
          setStatus(`Consuming ${info.symbol} mint note...`);
          const consumeRequest = client.newConsumeTransactionRequest(toConsume);
          await submitTx(AccountId.fromHex(walletId), consumeRequest);
          await client.syncState();
          setBalances(await readBalances(walletId));
        });
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setStatus(null);
      }
    },
    [isReady, walletId, deployment, client, runExclusive, submitTx, readBalances],
  );

  return {
    walletId,
    balances,
    isBusy: status !== null,
    status,
    error,
    ensureWallet,
    fund,
    refreshBalances,
  };
}
