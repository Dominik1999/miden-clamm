import type { ClammDeployment } from "@/lib/clamm/deployment";
import type { PoolState } from "@/hooks/clamm/usePoolState";
import type { TrackedNoteWithStatus, ActivityItem } from "@/hooks/clamm/useNoteLifecycle";
import { Q96 } from "@/lib/clamm/price";

export const DEPLOYMENT: ClammDeployment = {
  network: { rpcUrl: "http://localhost:57291", proverUrl: "http://localhost:50051" },
  pool: {
    id: "0x9e54030f993b3311620ba6a47f6e2f",
    feePips: 3000,
    tickSpacing: 60,
    initialTick: 0,
  },
  token0: { id: "0xbe7384179a6bd43176aae2ef7e20d6", symbol: "TKA", decimals: 6, devSecretKeyHex: "aa" },
  token1: { id: "0xa499f7c830ca55517ae8d824651849", symbol: "TKB", decimals: 6, devSecretKeyHex: "bb" },
  roots: { swap: "0x1", mint: "0x2", burn: "0x3", collect: "0x4", p2id: "0x5" },
};

export const POOL_STATE: PoolState = {
  sqrtPriceX96: Q96, // price exactly 1.0
  tick: 0,
  liquidity: 11_000_000_000_000n,
  feePips: 3000,
  tickSpacing: 60,
  blockHeight: 1000,
};

export const WALLET_ID = "0x0966dc36e19b3a5168ee7be5fceddd";

export function trackedNote(
  overrides: Partial<TrackedNoteWithStatus> = {},
): TrackedNoteWithStatus {
  return {
    id: "0xnote1",
    kind: "swap",
    senderHex: WALLET_ID,
    bytesHex: "00",
    serial: ["1", "2", "3", "4"],
    deadline: 1100,
    createdAtBlock: 1000,
    submittedAt: 1_700_000_000_000,
    direction: 0,
    inputFaucetHex: DEPLOYMENT.token0.id,
    outputFaucetHex: DEPLOYMENT.token1.id,
    summary: "Swap TKA -> TKB, in 1000000, min out 992015",
    status: "pending",
    ...overrides,
  };
}

export function activityItem(overrides: Partial<ActivityItem> = {}): ActivityItem {
  return {
    id: "0xp2id1",
    assets: [{ faucetHex: DEPLOYMENT.token1.id, amount: 997_000n }],
    sourceNoteId: "0xnote1",
    salt: 0,
    ...overrides,
  };
}
