import { expect } from 'chai';
import BN from 'bn.js';
import {
  I80F48_FRACTIONAL_BITS,
  mantissaToDecimal,
  mulScale,
  divScale,
  encodeOptionU16,
  encodeOptionU32,
} from '../src/utils/numbers';

describe('numbers utils', () => {
  it('mantissaToDecimal: 1.0 round-trip', () => {
    const one = new BN(1).shln(I80F48_FRACTIONAL_BITS);
    expect(mantissaToDecimal(one)).to.equal(1);
  });

  it('mantissaToDecimal: 0.5 round-trip', () => {
    const half = new BN(1).shln(I80F48_FRACTIONAL_BITS - 1);
    expect(mantissaToDecimal(half)).to.equal(0.5);
  });

  it('mulScale is the inverse of divScale (modulo flooring)', () => {
    const atoms = new BN(1_000_000);
    const ratio = new BN(15).shln(I80F48_FRACTIONAL_BITS - 1); // 7.5
    const product = mulScale(atoms, ratio);
    expect(product.toString()).to.equal('7500000');
    const back = divScale(product, ratio);
    expect(back.toString()).to.equal('1000000');
  });

  it('encodeOptionU32: None → [0]', () => {
    expect(Array.from(encodeOptionU32(null))).to.deep.equal([0]);
  });

  it('encodeOptionU32: Some(42) → [1, 42, 0, 0, 0]', () => {
    expect(Array.from(encodeOptionU32(42))).to.deep.equal([1, 42, 0, 0, 0]);
  });

  it('encodeOptionU16: Some(0x1234) → [1, 0x34, 0x12]', () => {
    expect(Array.from(encodeOptionU16(0x1234))).to.deep.equal([1, 0x34, 0x12]);
  });
});
