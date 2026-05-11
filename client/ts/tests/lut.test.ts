import { expect } from 'chai';
import { Keypair, PublicKey, SystemProgram, TransactionInstruction } from '@solana/web3.js';
import {
  dedupKeys,
  uniqueKeysFromIxs,
  chunkKeys,
  lutDeltaKeys,
  EXTEND_LUT_CHUNK_SIZE,
} from '../src/helpers/lut';
import {
  countUniqueKeys,
  pickTxMode,
  LEGACY_TX_MAX_KEYS,
} from '../src/utils/transaction';

const A = new PublicKey('11111111111111111111111111111112');
const B = new PublicKey('11111111111111111111111111111113');
const C = new PublicKey('11111111111111111111111111111114');

describe('lut utilities', () => {
  it('dedupKeys preserves first-seen order', () => {
    const out = dedupKeys([A, B, A, C, B]);
    expect(out.map((k) => k.toBase58())).to.deep.equal([
      A.toBase58(),
      B.toBase58(),
      C.toBase58(),
    ]);
  });

  it('chunkKeys splits into windows of the requested size', () => {
    const keys = Array.from({ length: 35 }, () => Keypair.generate().publicKey);
    const chunks = chunkKeys(keys, EXTEND_LUT_CHUNK_SIZE);
    expect(chunks.length).to.equal(2);
    expect(chunks[0].length).to.equal(EXTEND_LUT_CHUNK_SIZE);
    expect(chunks[1].length).to.equal(5);
  });

  it('chunkKeys defaults to EXTEND_LUT_CHUNK_SIZE', () => {
    const keys = Array.from({ length: 50 }, () => Keypair.generate().publicKey);
    const chunks = chunkKeys(keys);
    expect(chunks.length).to.equal(Math.ceil(50 / EXTEND_LUT_CHUNK_SIZE));
  });

  it('chunkKeys rejects nonsensical chunk sizes', () => {
    expect(() => chunkKeys([A], 0)).to.throw();
  });

  it('uniqueKeysFromIxs collects from keys + programIds, no duplicates', () => {
    const ix1: TransactionInstruction = {
      programId: A,
      keys: [
        { pubkey: B, isWritable: true, isSigner: false },
        { pubkey: C, isWritable: false, isSigner: false },
      ],
      data: Buffer.alloc(0),
    } as TransactionInstruction;
    const ix2: TransactionInstruction = {
      programId: SystemProgram.programId,
      keys: [{ pubkey: B, isWritable: false, isSigner: false }],
      data: Buffer.alloc(0),
    } as TransactionInstruction;
    const out = uniqueKeysFromIxs([ix1, ix2]);
    const base58 = out.map((k) => k.toBase58());
    // A (programId), B, C, SystemProgram — in first-seen order
    expect(base58).to.deep.equal([
      A.toBase58(),
      B.toBase58(),
      C.toBase58(),
      SystemProgram.programId.toBase58(),
    ]);
  });

  it('lutDeltaKeys returns only the keys not already present', () => {
    const existing = {
      state: { addresses: [A, B] },
    } as unknown as Parameters<typeof lutDeltaKeys>[0];
    const out = lutDeltaKeys(existing, [A, B, C, A]);
    expect(out.map((k) => k.toBase58())).to.deep.equal([C.toBase58()]);
  });
});

describe('tx composer auto-mode', () => {
  function ixWithKeys(n: number): TransactionInstruction {
    return {
      programId: A,
      keys: Array.from({ length: n }, () => ({
        pubkey: Keypair.generate().publicKey,
        isWritable: false,
        isSigner: false,
      })),
      data: Buffer.alloc(0),
    } as TransactionInstruction;
  }

  it('countUniqueKeys counts the programId + every account', () => {
    const ix = ixWithKeys(5);
    expect(countUniqueKeys([ix])).to.equal(6); // 5 keys + 1 programId
  });

  it('pickTxMode prefers legacy under the threshold', () => {
    const small = ixWithKeys(LEGACY_TX_MAX_KEYS - 5);
    expect(pickTxMode([small])).to.equal('legacy');
  });

  it('pickTxMode switches to v0 above the threshold', () => {
    const big = ixWithKeys(LEGACY_TX_MAX_KEYS + 10);
    expect(pickTxMode([big])).to.equal('v0');
  });

  it('pickTxMode picks v0 whenever LUTs are supplied', () => {
    const small = ixWithKeys(3);
    const fakeLut = { state: { addresses: [] } } as never;
    expect(pickTxMode([small], [fakeLut])).to.equal('v0');
  });
});
