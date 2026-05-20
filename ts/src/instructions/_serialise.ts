/**
 * Tiny hand-rolled borsh-compatible writer.
 *
 * The on-chain processors use `borsh::BorshDeserialize` (or hand-rolled
 * `bytemuck`-style reads) over the instruction-data tail. For primitives —
 * u8 / u16 / u32 / u64 / u128 / i64 / bool / Pubkey / `Option<T>` — borsh
 * is just little-endian + a 1-byte `Some`/`None` tag for options. We
 * keep the writer dependency-free so the bytes are easy to audit.
 */
import { PublicKey } from '@solana/web3.js';
import BN from 'bn.js';

export class Writer {
  private buf: number[] = [];

  toBuffer(): Buffer {
    return Buffer.from(this.buf);
  }

  u8(value: number): this {
    if (!Number.isInteger(value) || value < 0 || value > 0xff) {
      throw new RangeError(`Writer.u8: out of range ${value}`);
    }
    this.buf.push(value);
    return this;
  }

  bool(value: boolean): this {
    return this.u8(value ? 1 : 0);
  }

  u16(value: number): this {
    if (!Number.isInteger(value) || value < 0 || value > 0xffff) {
      throw new RangeError(`Writer.u16: out of range ${value}`);
    }
    this.buf.push(value & 0xff, (value >> 8) & 0xff);
    return this;
  }

  u32(value: number): this {
    if (!Number.isInteger(value) || value < 0 || value > 0xffff_ffff) {
      throw new RangeError(`Writer.u32: out of range ${value}`);
    }
    const b = Buffer.alloc(4);
    b.writeUInt32LE(value, 0);
    this.buf.push(...b);
    return this;
  }

  u64(value: bigint | BN | number): this {
    const big = toBigInt(value);
    if (big < 0n || big > 0xffff_ffff_ffff_ffffn) {
      throw new RangeError(`Writer.u64: out of range ${big}`);
    }
    const b = Buffer.alloc(8);
    b.writeBigUInt64LE(big, 0);
    this.buf.push(...b);
    return this;
  }

  i64(value: bigint | BN | number): this {
    const big = toBigInt(value);
    const min = -(1n << 63n);
    const max = (1n << 63n) - 1n;
    if (big < min || big > max) {
      throw new RangeError(`Writer.i64: out of range ${big}`);
    }
    const b = Buffer.alloc(8);
    b.writeBigInt64LE(big, 0);
    this.buf.push(...b);
    return this;
  }

  u128(value: bigint | BN | number): this {
    const big = toBigInt(value);
    if (big < 0n || big >= 1n << 128n) {
      throw new RangeError(`Writer.u128: out of range ${big}`);
    }
    const lo = big & 0xffff_ffff_ffff_ffffn;
    const hi = big >> 64n;
    const b = Buffer.alloc(16);
    b.writeBigUInt64LE(lo, 0);
    b.writeBigUInt64LE(hi, 8);
    this.buf.push(...b);
    return this;
  }

  pubkey(key: PublicKey): this {
    this.buf.push(...key.toBuffer());
    return this;
  }

  /** Borsh `Option<u8>` — 1-byte tag + payload if Some. */
  optionU8(v: number | null | undefined): this {
    if (v === null || v === undefined) return this.u8(0);
    return this.u8(1).u8(v);
  }
  optionU16(v: number | null | undefined): this {
    if (v === null || v === undefined) return this.u8(0);
    return this.u8(1).u16(v);
  }
  optionU32(v: number | null | undefined): this {
    if (v === null || v === undefined) return this.u8(0);
    return this.u8(1).u32(v);
  }
  optionU64(v: bigint | BN | number | null | undefined): this {
    if (v === null || v === undefined) return this.u8(0);
    return this.u8(1).u64(v);
  }
  optionDataIndex(v: number | null | undefined): this {
    return this.optionU32(v);
  }
}

function toBigInt(v: bigint | BN | number): bigint {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number') {
    if (!Number.isFinite(v)) throw new RangeError(`expected finite, got ${v}`);
    return BigInt(Math.trunc(v));
  }
  return BigInt(v.toString());
}
