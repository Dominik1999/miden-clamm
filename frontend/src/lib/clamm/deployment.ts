// The deployment descriptor written by the Rust exporter's `--deploy` mode
// (project-template `export_web_artifacts`): pool + faucet account ids,
// pool parameters, note-script roots, and local network URLs.

export interface ClammTokenInfo {
  id: string;
  symbol: string;
  decimals: number;
  /** DEV-ONLY: serialized faucet AuthSecretKey (hex) so the browser can mint test tokens. */
  devSecretKeyHex?: string;
}

export interface ClammDeployment {
  network: { rpcUrl: string; proverUrl: string };
  pool: { id: string; feePips: number; tickSpacing: number; initialTick: number };
  token0: ClammTokenInfo;
  token1: ClammTokenInfo;
  roots: { swap: string; mint: string; burn: string; collect: string; p2id: string };
}

/** Storage slot names of the MASM pool component (clamm-pool-masm). */
export const POOL_SLOTS = {
  poolConfig: "clamm_pool::clamm_pool::pool_config",
  poolParams: "clamm_pool::clamm_pool::pool_params",
  sqrtPrice: "clamm_pool::clamm_pool::sqrt_price",
  poolState: "clamm_pool::clamm_pool::pool_state",
  liquidity: "clamm_pool::clamm_pool::liquidity",
  positions: "clamm_pool::clamm_pool::positions",
} as const;

const ACCOUNT_ID_RE = /^0x[0-9a-fA-F]{30}$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function parseToken(value: unknown, label: string): ClammTokenInfo {
  if (!isRecord(value)) throw new Error(`deployment.${label} missing`);
  const { id, symbol, decimals, devSecretKeyHex } = value;
  if (typeof id !== "string" || !ACCOUNT_ID_RE.test(id)) {
    throw new Error(`deployment.${label}.id is not a valid account id`);
  }
  if (typeof symbol !== "string" || symbol.length === 0) {
    throw new Error(`deployment.${label}.symbol missing`);
  }
  if (typeof decimals !== "number" || !Number.isInteger(decimals) || decimals < 0) {
    throw new Error(`deployment.${label}.decimals invalid`);
  }
  return {
    id,
    symbol,
    decimals,
    devSecretKeyHex: typeof devSecretKeyHex === "string" ? devSecretKeyHex : undefined,
  };
}

/** Validates and normalizes a parsed deployment.json object. Throws on malformed input. */
export function parseDeployment(value: unknown): ClammDeployment {
  if (!isRecord(value)) throw new Error("deployment descriptor is not an object");
  const { network, pool, token0, token1, roots } = value;
  if (!isRecord(network) || typeof network.rpcUrl !== "string" || typeof network.proverUrl !== "string") {
    throw new Error("deployment.network missing rpcUrl/proverUrl");
  }
  if (!isRecord(pool) || typeof pool.id !== "string" || !ACCOUNT_ID_RE.test(pool.id)) {
    throw new Error("deployment.pool.id is not a valid account id");
  }
  const { feePips, tickSpacing, initialTick } = pool;
  if (
    typeof feePips !== "number" ||
    typeof tickSpacing !== "number" ||
    typeof initialTick !== "number" ||
    tickSpacing <= 0
  ) {
    throw new Error("deployment.pool parameters invalid");
  }
  if (!isRecord(roots)) throw new Error("deployment.roots missing");
  for (const key of ["swap", "mint", "burn", "collect", "p2id"] as const) {
    if (typeof roots[key] !== "string") throw new Error(`deployment.roots.${key} missing`);
  }
  return {
    network: { rpcUrl: network.rpcUrl, proverUrl: network.proverUrl },
    pool: { id: pool.id, feePips, tickSpacing, initialTick },
    token0: parseToken(token0, "token0"),
    token1: parseToken(token1, "token1"),
    roots: roots as ClammDeployment["roots"],
  };
}
