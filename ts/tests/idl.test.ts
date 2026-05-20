import { describe, expect, it } from 'vitest';

import idl from '../idl/ydelta.json';

describe('ydelta idl shape', () => {
  it('keeps instruction accounts machine-readable', () => {
    const liquidate = idl.instructions.find((ix) => ix.name === 'LiquidateLoan');
    expect(Array.isArray(liquidate?.accounts)).toBe(true);

    const processMatched = idl.instructions.find((ix) => ix.name === 'ProcessMatchedLoan');
    expect(processMatched).toBeDefined();
    expect('optionalVaultSettleAccounts' in (processMatched as Record<string, unknown>)).toBe(false);
  });

  it('marks claim repayment cranker_refund as required', () => {
    const claimRepayment = idl.instructions.find(
      (ix) => ix.name === 'ClaimRepaymentForRiskProfile',
    );
    expect(claimRepayment).toBeDefined();
    const refund = claimRepayment?.accounts.find((acc) => acc.name === 'cranker_refund');
    expect(refund).toBeDefined();
    expect('optional' in (refund as Record<string, unknown>)).toBe(false);
  });
});
