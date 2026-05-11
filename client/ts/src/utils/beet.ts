import * as beet from '@metaplex-foundation/beet';
import { publicKey as beetPublicKey } from '@metaplex-foundation/beet-solana';
import BN from 'bn.js';

export { beet, beetPublicKey };

/** 16-byte little-endian unsigned integer, exposed as bn.js BN. */
export const u128: beet.FixedSizeBeet<BN> = {
  write(buf, offset, value) {
    const bytes = value.toArrayLike!(Buffer, 'le', 16);
    bytes.copy(buf, offset);
  },
  read(buf, offset) {
    return new BN(buf.subarray(offset, offset + 16), 'le');
  },
  byteSize: 16,
  description: 'u128',
};

/** 16-byte signed I80F48 fixed-point, exposed as bn.js BN of the raw u128 mantissa.
 *  Mirrors the on-chain `wrapped_i80f48_to_u128` adapter in
 *  `programs/ydelta/src/protocol/marginfi.rs`: negative wire values are clamped
 *  to 0 (a transiently-negative WrappedI80F48 in marginfi state is treated as
 *  a degenerate zero). The read therefore sign-extends i128 first and clamps. */
export const i80f48: beet.FixedSizeBeet<BN> = {
  write(buf, offset, value) {
    const bytes = value.toArrayLike!(Buffer, 'le', 16);
    bytes.copy(buf, offset);
  },
  read(buf, offset) {
    // Read as unsigned u128 then sign-extend if the high bit is set
    // (i128 in two's complement). Clamp negative values to 0 — matching
    // the on-chain adapter — so downstream multipliers can't produce
    // non-physical negative atom counts.
    const u = new BN(buf.subarray(offset, offset + 16), 'le');
    if (u.testn(127)) {
      // Negative i128 → clamp.
      return new BN(0);
    }
    return u;
  },
  byteSize: 16,
  description: 'I80F48',
};

/** hypertree::DataIndex newtype — u32. */
export const dataIndex: beet.FixedSizeBeet<number> = beet.u32 as beet.FixedSizeBeet<number>;

/** `Option<u32>` in borsh layout (1-byte tag + u32 when Some). Used for ix-arg index hints. */
export const optionDataIndex: beet.FixableBeet<number | null> = beet.coption(beet.u32);

/** `Option<u16>` in borsh layout. */
export const optionU16: beet.FixableBeet<number | null> = beet.coption(beet.u16);
