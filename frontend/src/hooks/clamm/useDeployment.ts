import { useEffect, useState } from "react";
import { CLAMM_DEPLOYMENT_URL } from "@/config";
import { parseDeployment, type ClammDeployment } from "@/lib/clamm/deployment";

export interface DeploymentResult {
  deployment: ClammDeployment | null;
  isLoading: boolean;
  /** Set when the descriptor exists but is malformed (missing file => deployment stays null). */
  error: string | null;
}

/**
 * Loads /packages/clamm/deployment.json (written by the Rust exporter's
 * `--deploy` mode). A missing file is a normal state — the UI then shows
 * deploy instructions instead of the app.
 */
export function useDeployment(): DeploymentResult {
  const [state, setState] = useState<DeploymentResult>({
    deployment: null,
    isLoading: true,
    error: null,
  });

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await fetch(CLAMM_DEPLOYMENT_URL, { cache: "no-store" });
        if (!res.ok) {
          if (!cancelled) setState({ deployment: null, isLoading: false, error: null });
          return;
        }
        // Vite's dev server serves index.html for unknown paths — guard on
        // content type so a missing file doesn't parse as HTML.
        const text = await res.text();
        let json: unknown;
        try {
          json = JSON.parse(text);
        } catch {
          if (!cancelled) setState({ deployment: null, isLoading: false, error: null });
          return;
        }
        const deployment = parseDeployment(json);
        if (!cancelled) setState({ deployment, isLoading: false, error: null });
      } catch (err) {
        if (!cancelled) {
          setState({
            deployment: null,
            isLoading: false,
            error: err instanceof Error ? err.message : String(err),
          });
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return state;
}
