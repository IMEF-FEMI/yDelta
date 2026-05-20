/**
 * Low-level `DataView` primitives shared by every zero-copy decoder.
 *
 * yDelta's account bodies are `bytemuck #[repr(C)]` Pod — raw byte layouts,
 * NOT borsh. Every reader here is a one-shot LE read from a fixed offset;
 * the `// @N` comments in the Rust struct definitions are the authoritative
 * byte map.
 */
import { PublicKey } from '@solana/web3.js';

/** Construct a DataView over a byte slice (Buffer / Uint8Array / ArrayBuffer). */
export function view(bytes: Uint8Array | Buffer): DataView {
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}

export const readU8 = (dv: DataView, off: number): number => dv.getUint8(off);
export const readI8 = (dv: DataView, off: number): number => dv.getInt8(off);
export const readU16 = (dv: DataView, off: number): number => dv.getUint16(off, true);
export const readU32 = (dv: DataView, off: number): number => dv.getUint32(off, true);
export const readI64 = (dv: DataView, off: number): bigint => dv.getBigInt64(off, true);
export const readU64 = (dv: DataView, off: number): bigint => dv.getBigUint64(off, true);

/** Little-endian u128 (16 bytes) → bigint. Solana / marginfi fp48 share-values use this. */
export function readU128(dv: DataView, off: number): bigint {
  const lo = dv.getBigUint64(off, true);
  const hi = dv.getBigUint64(off + 8, true);
  return (hi << 64n) | lo;
}

/** A 32-byte Pubkey at `off`. */
export function readPubkey(dv: DataView, off: number): PublicKey {
  const bytes = new Uint8Array(dv.buffer, dv.byteOffset + off, 32);
  return new PublicKey(bytes);
}

/** Hypertree NIL sentinel — same as `state.constants.NIL_DATA_INDEX`. */
export const NIL_INDEX = 0xffff_ffff;

/** True when an RB-tree / free-list index is empty. */
export const isNil = (idx: number): boolean => idx === NIL_INDEX;
