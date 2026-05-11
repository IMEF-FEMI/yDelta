import BN from 'bn.js';

export const BPS_DENOMINATOR = 10_000;

/** I80F48 has 48 fractional bits. */
export const I80F48_FRACTIONAL_BITS = 48;
export const I80F48_DENOMINATOR = new BN(1).shln(I80F48_FRACTIONAL_BITS);

/** Convert a raw u128 mantissa (I80F48 stored as u128 after the marginfi
 *  adapter's clamp-negative-to-zero step) into a JS number with
 *  `fractionalBits` of precision. Routes through `BigInt` so the integer
 *  part doesn't lose precision when it exceeds `2^53`. */
export function mantissaToDecimal(
  mantissa: BN,
  fractionalBits: number = I80F48_FRACTIONAL_BITS,
): number {
  const scale = 1n << BigInt(fractionalBits);
  const m = BigInt(mantissa.toString());
  const integer = m / scale;
  const fractional = m % scale;
  // Number(bigint) clamps; pair with fractional/scale for the residue.
  return Number(integer) + Number(fractional) / Number(scale);
}

/** Multiply a u64 atom count by an I80F48 share price (or any fp48 ratio) and
 *  return the result as a u64 BN. Floor rounding. */
export function mulScale(atoms: BN, fp48: BN): BN {
  return atoms.mul(fp48).shrn(I80F48_FRACTIONAL_BITS);
}

/** Inverse of `mulScale`: shares = atoms × 2^48 / sharePrice. */
export function divScale(atoms: BN, fp48: BN): BN {
  if (fp48.isZero()) {
    throw new Error('divScale: fp48 share price is zero');
  }
  return atoms.shln(I80F48_FRACTIONAL_BITS).div(fp48);
}

export function bpsToFraction(bps: number): number {
  return bps / BPS_DENOMINATOR;
}

export function toBN(value: number | bigint | BN): BN {
  if (BN.isBN(value)) return value;
  if (typeof value === 'bigint') return new BN(value.toString());
  return new BN(value);
}

/** Borsh-Option<T> on the wire is one byte (0/1) + (Some -> T). This returns
 *  the encoded form for an `Option<u32>` data hint. */
export function encodeOptionU32(v: number | null): Buffer {
  if (v == null) return Buffer.from([0]);
  if (!Number.isInteger(v) || v < 0 || v > 0xffff_ffff) {
    throw new Error(`encodeOptionU32: value ${v} out of u32 range`);
  }
  const out = Buffer.alloc(5);
  out.writeUInt8(1, 0);
  out.writeUInt32LE(v, 1);
  return out;
}

export function encodeOptionU16(v: number | null): Buffer {
  if (v == null) return Buffer.from([0]);
  if (!Number.isInteger(v) || v < 0 || v > 0xffff) {
    throw new Error(`encodeOptionU16: value ${v} out of u16 range`);
  }
  const out = Buffer.alloc(3);
  out.writeUInt8(1, 0);
  out.writeUInt16LE(v, 1);
  return out;
}
