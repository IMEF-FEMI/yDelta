/**
 * Tests for `instructions/_serialise.ts`. The writer is borsh-compatible for
 * the primitives we use — verify each one against hand-computed bytes.
 */
import { describe, expect, it } from 'vitest';
import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';

import { Writer } from '../src/instructions/_serialise.js';

describe('Writer primitives', () => {
  it('encodes u8', () => {
    expect(new Writer().u8(0).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().u8(255).toBuffer()).toEqual(Buffer.from([0xff]));
  });

  it('rejects out-of-range u8', () => {
    expect(() => new Writer().u8(-1)).toThrow(RangeError);
    expect(() => new Writer().u8(256)).toThrow(RangeError);
  });

  it('encodes bool', () => {
    expect(new Writer().bool(false).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().bool(true).toBuffer()).toEqual(Buffer.from([1]));
  });

  it('encodes u16 little-endian', () => {
    expect(new Writer().u16(0x1234).toBuffer()).toEqual(Buffer.from([0x34, 0x12]));
    expect(new Writer().u16(0xffff).toBuffer()).toEqual(Buffer.from([0xff, 0xff]));
  });

  it('encodes u32 little-endian', () => {
    expect(new Writer().u32(0x01020304).toBuffer()).toEqual(
      Buffer.from([0x04, 0x03, 0x02, 0x01]),
    );
  });

  it('encodes u64 little-endian (bigint, BN, number)', () => {
    const expected = Buffer.from([0x40, 0xe2, 0x01, 0, 0, 0, 0, 0]); // 123456
    expect(new Writer().u64(123_456n).toBuffer()).toEqual(expected);
    expect(new Writer().u64(new BN(123_456)).toBuffer()).toEqual(expected);
    expect(new Writer().u64(123_456).toBuffer()).toEqual(expected);
  });

  it('encodes i64 with sign', () => {
    const neg = new Writer().i64(-1).toBuffer();
    expect(neg).toEqual(Buffer.from([0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]));
  });

  it('encodes u128 little-endian', () => {
    const one = new Writer().u128(1n).toBuffer();
    const expected = Buffer.alloc(16);
    expected[0] = 1;
    expect(one).toEqual(expected);

    const max = new Writer().u128((1n << 128n) - 1n).toBuffer();
    expect(max).toEqual(Buffer.alloc(16, 0xff));
  });

  it('encodes pubkey as raw 32 bytes', () => {
    const pk = new PublicKey('11111111111111111111111111111111');
    expect(new Writer().pubkey(pk).toBuffer()).toEqual(pk.toBuffer());
  });

  it('encodes Option<u16> with 1-byte tag', () => {
    expect(new Writer().optionU16(null).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().optionU16(undefined).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().optionU16(0x1234).toBuffer()).toEqual(
      Buffer.from([1, 0x34, 0x12]),
    );
  });

  it('encodes Option<u32> / Option<u64>', () => {
    expect(new Writer().optionU32(null).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().optionU32(0x01020304).toBuffer()).toEqual(
      Buffer.from([1, 0x04, 0x03, 0x02, 0x01]),
    );
    expect(new Writer().optionU64(null).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().optionU64(1n).toBuffer()).toEqual(
      Buffer.from([1, 1, 0, 0, 0, 0, 0, 0, 0]),
    );
  });

  it('encodes Option<u8>', () => {
    expect(new Writer().optionU8(null).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().optionU8(undefined).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().optionU8(0x7f).toBuffer()).toEqual(Buffer.from([1, 0x7f]));
  });

  it('encodes the seat-index hint as an Option<u32> (optionDataIndex)', () => {
    // v1 place/cancel/update-order builders pass `seatIndexHint` through
    // `optionDataIndex`, which is a plain borsh `Option<u32>`.
    expect(new Writer().optionDataIndex(null).toBuffer()).toEqual(Buffer.from([0]));
    expect(new Writer().optionDataIndex(0x01020304).toBuffer()).toEqual(
      Buffer.from([1, 0x04, 0x03, 0x02, 0x01]),
    );
  });

  it('chains writes in order', () => {
    const out = new Writer()
      .u8(0xff)
      .u16(0x1234)
      .u32(0xdeadbeef)
      .bool(true)
      .toBuffer();
    expect(out).toEqual(Buffer.from([0xff, 0x34, 0x12, 0xef, 0xbe, 0xad, 0xde, 0x01]));
  });
});
