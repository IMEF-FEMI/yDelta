import { expect } from 'chai';
import {
  errorFromCode,
  errorFromName,
  InvalidMarketParametersError,
  GlobalPausedError,
  BorrowerLtvOverInitError,
} from '../src/ydelta/errors';

describe('YdeltaError mapping', () => {
  it('maps code 0 → InvalidMarketParameters', () => {
    const e = errorFromCode(0);
    expect(e).to.be.ok;
    expect(e!.name).to.equal('InvalidMarketParameters');
  });

  it('maps the last variant (58 → BorrowerLtvOverInit)', () => {
    const e = errorFromCode(58);
    expect(e).to.be.ok;
    expect(e!.name).to.equal('BorrowerLtvOverInit');
  });

  it('returns null for unknown codes', () => {
    expect(errorFromCode(9999)).to.equal(null);
  });

  it('maps by name', () => {
    const e = errorFromName('GlobalPaused');
    expect(e).to.be.ok;
    expect(e!.code).to.equal(55);
  });

  it('exported error classes carry their code', () => {
    expect(new InvalidMarketParametersError().code).to.equal(0);
    expect(new GlobalPausedError().code).to.equal(55);
    expect(new BorrowerLtvOverInitError().code).to.equal(58);
  });
});
