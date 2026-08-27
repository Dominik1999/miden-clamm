import { describe, it, expect } from "vitest";
import {
  serializeClammNote,
  randomSerial,
  bytesToHex,
  hexToBytes,
  type ClammNoteParams,
} from "@/lib/clamm/noteBytes";
import { loadGolden, loadScriptBytes, type GoldenNote } from "./golden";

const golden = loadGolden();

function paramsFromGolden(note: GoldenNote): ClammNoteParams {
  return {
    senderHex: note.senderHex,
    tag: note.tag,
    assets: note.assets.map((a) => ({ faucetHex: a.faucet, amount: BigInt(a.amount) })),
    scriptBytes: loadScriptBytes(note.kind),
    storage: note.storage.map((s) => BigInt(s)),
    serial: note.serial.map((s) => BigInt(s)) as [bigint, bigint, bigint, bigint],
    attachmentWord: note.attachmentWord.map((w) => BigInt(w)) as [
      bigint,
      bigint,
      bigint,
      bigint,
    ],
  };
}

describe("serializeClammNote", () => {
  it.each(golden.notes.map((n) => [n.kind, n] as const))(
    "serializes the golden %s note byte-for-byte identically to Rust",
    (_kind, note) => {
      const bytes = serializeClammNote(paramsFromGolden(note));
      expect(bytesToHex(bytes)).toBe(note.bytesHex);
    },
  );

  it("serializes an attachment-less note by writing a zero attachments count", () => {
    const note = golden.notes.find((n) => n.kind === "swap")!;
    const withAttachment = serializeClammNote(paramsFromGolden(note));
    const without = serializeClammNote({ ...paramsFromGolden(note), attachmentWord: null });
    // Attachment section: 1 count byte is shared; the attachment itself is
    // scheme u16 + num_words u8 + 32-byte word = 35 bytes.
    expect(withAttachment.length).toBe(without.length + 35);
    expect(without[without.length - 1]).toBe(0);
  });

  it("rejects out-of-field storage felts", () => {
    const note = golden.notes.find((n) => n.kind === "collect")!;
    const params = paramsFromGolden(note);
    params.storage = [2n ** 64n - 1n]; // >= field modulus
    expect(() => serializeClammNote(params)).toThrow(/not a canonical field element/);
  });

  it("rejects oversized asset amounts", () => {
    const note = golden.notes.find((n) => n.kind === "swap")!;
    const params = paramsFromGolden(note);
    params.assets = [{ faucetHex: note.assets[0].faucet, amount: 2n ** 64n }];
    expect(() => serializeClammNote(params)).toThrow(/u64 out of range/);
  });
});

describe("randomSerial", () => {
  it("produces 4 canonical felts and varies across calls", () => {
    const a = randomSerial();
    const b = randomSerial();
    expect(a).toHaveLength(4);
    for (const felt of [...a, ...b]) {
      expect(felt).toBeGreaterThanOrEqual(0n);
      expect(felt).toBeLessThan(2n ** 63n);
    }
    expect(a.join(",")).not.toBe(b.join(","));
  });
});

describe("hex helpers", () => {
  it("round-trips bytes through hex", () => {
    const bytes = new Uint8Array([0, 1, 0xab, 0xff, 42]);
    expect(hexToBytes(bytesToHex(bytes))).toEqual(bytes);
  });

  it("accepts an optional 0x prefix", () => {
    expect(hexToBytes("0x00ff")).toEqual(new Uint8Array([0, 255]));
  });

  it("rejects invalid hex", () => {
    expect(() => hexToBytes("abc")).toThrow(/invalid hex/);
    expect(() => hexToBytes("zz")).toThrow(/invalid hex/);
  });
});
