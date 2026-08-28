// Pure-TS serializer for a complete Miden `Note` carrying note storage and a
// `NetworkAccountTarget` attachment.
//
// Why this exists: the 0.15 web SDK's JS `Note` constructor only builds
// attachment-less notes (`NoteMetadata`'s JS constructor is explicitly
// "metadata for a note with no attachments"), but the CLAMM pool notes MUST
// carry the scheme-2 NetworkAccountTarget attachment or the ntx-builder will
// never consume them. So we serialize the full note byte stream here —
// mirroring miden-protocol 0.15 `impl Serializable for Note`:
//
//   PartialNoteMetadata || NoteDetails || NoteAttachments
//
//   PartialNoteMetadata = note_type u8 || sender AccountId [u8;15] || tag u32 LE
//   NoteDetails         = NoteAssets || NoteRecipient
//   NoteAssets          = count u8 || per asset:
//                           composition u8 (1 = fungible)
//                           faucet AccountId [u8;15]
//                           amount u64 LE
//                           callback flag u8 (0 = disabled)
//   NoteRecipient       = NoteScript (verbatim exported bytes)
//                         || NoteStorage (count u16 LE || felts u64 LE)
//                         || serial Word (4 felts u64 LE)
//   NoteAttachments     = count u8 || per attachment:
//                           scheme u16 LE || (num_words - 1) u8 || words
//
// and hand the bytes to the WASM `Note.deserialize`, which validates the
// stream and recomputes the note id/commitments. Byte-exactness is enforced
// by unit tests against golden notes serialized by the Rust exporter.

import { accountIdBytes, felt } from "./felts";
import { NETWORK_ACCOUNT_TARGET_SCHEME } from "./encoding";

export interface ClammNoteAsset {
  faucetHex: string;
  amount: bigint;
}

export interface ClammNoteParams {
  senderHex: string;
  /** Note tag as u32: `accountTargetTag(poolHex)` — pool-targeted, required for ntx-builder discovery. */
  tag: number;
  assets: ClammNoteAsset[];
  /** Serialized NoteScript bytes (the exported `*.notescript` file contents). */
  scriptBytes: Uint8Array;
  /** Note storage felts. */
  storage: bigint[];
  /** Serial number: 4 felts. */
  serial: [bigint, bigint, bigint, bigint];
  /** Single-word attachment content, or null for an attachment-less note. */
  attachmentWord: [bigint, bigint, bigint, bigint] | null;
  /** Attachment scheme; defaults to NetworkAccountTarget (2). */
  attachmentScheme?: number;
}

class ByteSink {
  private chunks: number[] = [];

  u8(v: number): void {
    if (!Number.isInteger(v) || v < 0 || v > 0xff) throw new Error(`u8 out of range: ${v}`);
    this.chunks.push(v);
  }

  u16le(v: number): void {
    if (!Number.isInteger(v) || v < 0 || v > 0xffff) throw new Error(`u16 out of range: ${v}`);
    this.chunks.push(v & 0xff, (v >> 8) & 0xff);
  }

  u32le(v: number): void {
    if (!Number.isInteger(v) || v < 0 || v > 0xffffffff) {
      throw new Error(`u32 out of range: ${v}`);
    }
    this.chunks.push(v & 0xff, (v >> 8) & 0xff, (v >> 16) & 0xff, (v >>> 24) & 0xff);
  }

  u64le(v: bigint): void {
    if (v < 0n || v >= 2n ** 64n) throw new Error(`u64 out of range: ${v}`);
    for (let i = 0n; i < 8n; i++) {
      this.chunks.push(Number((v >> (8n * i)) & 0xffn));
    }
  }

  bytes(b: Uint8Array): void {
    for (const v of b) this.chunks.push(v);
  }

  feltLe(v: bigint): void {
    this.u64le(felt(v));
  }

  toUint8Array(): Uint8Array {
    return new Uint8Array(this.chunks);
  }
}

/** Serializes a complete Miden note (0.15 wire format). */
export function serializeClammNote(params: ClammNoteParams): Uint8Array {
  const sink = new ByteSink();

  // PartialNoteMetadata: note_type (1 = public) || sender || tag.
  sink.u8(1);
  sink.bytes(accountIdBytes(params.senderHex));
  sink.u32le(params.tag);

  // NoteDetails: assets then recipient.
  if (params.assets.length > 0xff) throw new Error("too many assets");
  sink.u8(params.assets.length);
  for (const asset of params.assets) {
    sink.u8(1); // AssetComposition::Fungible
    sink.bytes(accountIdBytes(asset.faucetHex));
    sink.u64le(asset.amount);
    sink.u8(0); // AssetCallbackFlag: disabled
  }

  // NoteRecipient: script || storage || serial.
  sink.bytes(params.scriptBytes);
  if (params.storage.length > 1024) throw new Error("too many storage felts");
  sink.u16le(params.storage.length);
  for (const item of params.storage) sink.feltLe(item);
  for (const s of params.serial) sink.feltLe(s);

  // NoteAttachments.
  if (params.attachmentWord === null) {
    sink.u8(0);
  } else {
    sink.u8(1);
    sink.u16le(params.attachmentScheme ?? NETWORK_ACCOUNT_TARGET_SCHEME);
    sink.u8(0); // num_words - 1
    for (const w of params.attachmentWord) sink.feltLe(w);
  }

  return sink.toUint8Array();
}

/** Generates a random 4-felt serial number (each felt a random u32 for safety). */
export function randomSerial(): [bigint, bigint, bigint, bigint] {
  const rand = () => {
    const buf = new Uint32Array(2);
    crypto.getRandomValues(buf);
    // Compose a value strictly below the field modulus: 63 random bits.
    return ((BigInt(buf[0]) << 31n) | (BigInt(buf[1]) >> 1n)) & (2n ** 63n - 1n);
  };
  return [rand(), rand(), rand(), rand()];
}

/** Hex helpers for persisting note bytes. */
export function bytesToHex(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}

export function hexToBytes(hex: string): Uint8Array {
  const clean = hex.startsWith("0x") ? hex.slice(2) : hex;
  if (clean.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(clean)) {
    throw new Error("invalid hex string");
  }
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(clean.slice(2 * i, 2 * i + 2), 16);
  }
  return out;
}
