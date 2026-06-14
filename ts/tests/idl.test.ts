import { describe, expect, it } from 'vitest';

import idl from '../idl/ydelta.json';

describe('ydelta idl shape', () => {
  it('has 45 instructions with contiguous tags 0..=44', () => {
    expect(idl.instructions).toHaveLength(45);
    const tags = idl.instructions.map((ix) => ix.tag).sort((a, b) => a - b);
    expect(tags).toEqual(Array.from({ length: 45 }, (_, i) => i));
  });

  it('keeps instruction accounts machine-readable', () => {
    const liquidate = idl.instructions.find((ix) => ix.name === 'LiquidateLoan');
    expect(Array.isArray(liquidate?.accounts)).toBe(true);

    const processMatched = idl.instructions.find((ix) => ix.name === 'ProcessMatchedLoan');
    expect(processMatched).toBeDefined();
    expect('optionalVaultSettleAccounts' in (processMatched as Record<string, unknown>)).toBe(false);
  });

  it('exposes the v1 sub-vault instruction surface (no legacy Profile names)', () => {
    const names = new Set(idl.instructions.map((ix) => ix.name));
    expect(names.has('ClaimRepaymentForSubVault')).toBe(true);
    expect(names.has('CreatePoolSubVault')).toBe(true);
    expect(names.has('CreatePrivateSubVault')).toBe(true);
    expect(names.has('PlaceOrderForSubVault')).toBe(true);
    expect(names.has('UpdateSubVault')).toBe(true);
    // The v1 user-side order surface.
    expect(names.has('CancelOrder')).toBe(true);
    expect(names.has('MatchCrank')).toBe(true);
    expect(names.has('UpdateOrder')).toBe(true);
    // No legacy Profile names survive.
    for (const name of names) {
      expect(name).not.toMatch(/Profile/);
    }
  });

  it('claim repayment is keyed on a sub-vault and no longer carries cranker_refund', () => {
    const claimRepayment = idl.instructions.find(
      (ix) => ix.name === 'ClaimRepaymentForSubVault',
    );
    expect(claimRepayment).toBeDefined();
    const refund = claimRepayment?.accounts.find((acc) => acc.name === 'cranker_refund');
    expect(refund).toBeUndefined();
  });

  it('renames the Profile error family and adds FallbackLtvInsufficient (56)', () => {
    const byCode = new Map(idl.errors.map((e) => [e.code, e.name]));
    expect(byCode.get(26)).toBe('SubVaultNotFound');
    expect(byCode.get(54)).toBe('SubVaultSunset');
    expect(byCode.get(55)).toBe('SubVaultNotSunset');
    expect(byCode.get(56)).toBe('FallbackLtvInsufficient');
    // No error name still references the old Profile concept.
    for (const e of idl.errors) {
      expect(e.name).not.toMatch(/Profile/);
    }
  });
});
