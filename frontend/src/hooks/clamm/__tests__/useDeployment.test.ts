import { renderHook, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useDeployment } from "@/hooks/clamm/useDeployment";

const VALID = {
  network: { rpcUrl: "http://localhost:57291", proverUrl: "http://localhost:50051" },
  pool: { id: "0x9e54030f993b3311620ba6a47f6e2f", feePips: 3000, tickSpacing: 60, initialTick: 0 },
  token0: { id: "0xbe7384179a6bd43176aae2ef7e20d6", symbol: "TKA", decimals: 6 },
  token1: { id: "0xa499f7c830ca55517ae8d824651849", symbol: "TKB", decimals: 6 },
  roots: { swap: "0x1", mint: "0x2", burn: "0x3", collect: "0x4", p2id: "0x5" },
};

function mockFetch(response: { ok: boolean; body: string }) {
  const fetchMock = vi.fn(async () => ({
    ok: response.ok,
    status: response.ok ? 200 : 404,
    text: async () => response.body,
  }));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

describe("useDeployment", () => {
  beforeEach(() => vi.clearAllMocks());
  afterEach(() => vi.unstubAllGlobals());

  it("loads and parses a valid descriptor", async () => {
    mockFetch({ ok: true, body: JSON.stringify(VALID) });
    const { result } = renderHook(() => useDeployment());
    expect(result.current.isLoading).toBe(true);
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.deployment?.pool.id).toBe(VALID.pool.id);
    expect(result.current.error).toBeNull();
  });

  it("treats a 404 as 'not deployed' (no error)", async () => {
    mockFetch({ ok: false, body: "" });
    const { result } = renderHook(() => useDeployment());
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.deployment).toBeNull();
    expect(result.current.error).toBeNull();
  });

  it("treats non-JSON (dev-server index.html fallback) as 'not deployed'", async () => {
    mockFetch({ ok: true, body: "<!doctype html><html></html>" });
    const { result } = renderHook(() => useDeployment());
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.deployment).toBeNull();
    expect(result.current.error).toBeNull();
  });

  it("reports malformed descriptors as errors", async () => {
    mockFetch({ ok: true, body: JSON.stringify({ ...VALID, pool: { id: "0xbad" } }) });
    const { result } = renderHook(() => useDeployment());
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.deployment).toBeNull();
    expect(result.current.error).toMatch(/pool\.id/);
  });

  it("reports network failures", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new Error("connection refused");
      }),
    );
    const { result } = renderHook(() => useDeployment());
    await waitFor(() => expect(result.current.isLoading).toBe(false));
    expect(result.current.error).toBe("connection refused");
  });
});
