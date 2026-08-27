// Loads the golden vectors exported by the Rust exporter
// (project-template: `cargo run --bin export_web_artifacts --release`).
// These files are checked into public/packages/clamm/ by the export step and
// make the TS encoders byte-exact against the validated Rust flow.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export interface GoldenAccount {
  hex: string;
  prefixFelt: string;
  suffixFelt: string;
}

export interface GoldenNote {
  kind: "swap" | "mint" | "burn" | "collect";
  senderHex: string;
  tag: number;
  serial: [string, string, string, string];
  storage: string[];
  assets: { faucet: string; amount: string }[];
  attachmentWord: [string, string, string, string];
  noteId: string;
  recipientDigest: string;
  bytesHex: string;
}

export interface Golden {
  roots: Record<"swap" | "mint" | "burn" | "collect" | "p2id", string>;
  accounts: Record<"user" | "pool" | "faucet0" | "faucet1", GoldenAccount>;
  tickOff: number;
  sqrtRatios: Record<string, string>;
  positionKeys: { owner: string; lower: number; upper: number; field: number; key: string }[];
  p2id: {
    salts: Record<string, string>;
    recipientDigestSalt0: string;
    storageFelts: string[];
  };
  notes: GoldenNote[];
}

// Vitest runs with cwd = frontend-template (vitest.config.ts location).
const packagesDir = resolve(process.cwd(), "public/packages/clamm");

export function loadGolden(): Golden {
  return JSON.parse(readFileSync(resolve(packagesDir, "golden.json"), "utf-8")) as Golden;
}

export function loadScriptBytes(kind: "swap" | "mint" | "burn" | "collect"): Uint8Array {
  return new Uint8Array(readFileSync(resolve(packagesDir, `${kind}.notescript`)));
}
