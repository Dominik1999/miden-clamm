import { Felt, Word } from "@miden-sdk/miden-sdk";

/** Generate a random 4-felt Word (used as note serial number). */
export function randomWord(): Word {
  const felts = Array.from({ length: 4 }, () =>
    new Felt(BigInt(Math.floor(Math.random() * 2 ** 32))),
  );
  return Word.newFromFelts(felts);
}
