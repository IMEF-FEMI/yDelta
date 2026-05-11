import { expect } from 'chai';
import {
  YdeltaInstructionTag,
  LAST_INSTRUCTION_TAG,
  GLOBAL_CONFIG_DISCRIMINANT,
  LOAN_FIXED_DISCRIMINANT,
  USER_ACCOUNT_FIXED_DISCRIMINANT,
  GLOBAL_VAULT_FIXED_DISCRIMINANT,
  MARKET_FIXED_DISCRIMINANT,
  NIL,
  isNotNil,
} from '../src/utils/discriminator';

describe('discriminator constants', () => {
  it('instruction tags are 0..=41 contiguous', () => {
    expect(LAST_INSTRUCTION_TAG).to.equal(41);
    const tagValues = Object.values(YdeltaInstructionTag)
      .filter((v): v is number => typeof v === 'number')
      .sort((a, b) => a - b);
    expect(tagValues[0]).to.equal(0);
    expect(tagValues[tagValues.length - 1]).to.equal(41);
    for (let i = 0; i < tagValues.length - 1; i++) {
      expect(tagValues[i + 1] - tagValues[i]).to.equal(1);
    }
    expect(tagValues.length).to.equal(42);
  });

  it('account discriminants decode to the expected ASCII tails', () => {
    // ASCII bytes of "Gc", "LN", "US", "Va", "MK"
    expect(GLOBAL_CONFIG_DISCRIMINANT & 0xffffn).to.equal(BigInt(('G'.charCodeAt(0) << 8) | 'c'.charCodeAt(0)));
    expect(LOAN_FIXED_DISCRIMINANT & 0xffffn).to.equal(BigInt(('L'.charCodeAt(0) << 8) | 'N'.charCodeAt(0)));
    expect(USER_ACCOUNT_FIXED_DISCRIMINANT & 0xffffn).to.equal(BigInt(('U'.charCodeAt(0) << 8) | 'S'.charCodeAt(0)));
    expect(GLOBAL_VAULT_FIXED_DISCRIMINANT & 0xffffn).to.equal(BigInt(('V'.charCodeAt(0) << 8) | 'a'.charCodeAt(0)));
    expect(MARKET_FIXED_DISCRIMINANT & 0xffffn).to.equal(BigInt(('M'.charCodeAt(0) << 8) | 'K'.charCodeAt(0)));
  });

  it('NIL sentinel for DataIndex', () => {
    expect(NIL).to.equal(0xffffffff);
    expect(isNotNil(NIL)).to.equal(false);
    expect(isNotNil(0)).to.equal(true);
  });
});
