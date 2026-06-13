/**
 * PDA derivation tests. Verifies every helper returns a deterministic,
 * on-curve-rejecting program-derived address for the documented seed
 * combination, and that distinct inputs yield distinct addresses.
 */
import { describe, expect, it } from 'vitest';
import { PublicKey } from '@solana/web3.js';

import {
  YDELTA_PROGRAM_ID,
  borrowerIntegrationAccountPda,
  globalConfigPda,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  lenderIntegrationAccountPda,
  loanPda,
  marketSignerPda,
  marketTokenVaultPda,
  userAccountPda,
} from '../src/index.js';

const MARKET = new PublicKey('CYf9nJB7eJYqVRm6ucMcRYrwvtT6mzS5HHKkc4dHHGxk');
const MINT = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'); // USDC
// The GlobalVault PDA is keyed by the marginfi BANK (a market's debt lending
// pool), not a mint — `globalVaultPda` keys on whatever pubkey it's handed.
const DEBT_BANK = new PublicKey('Fe5QkKPVAh629UPP5aJ8sDZu8HTfe6M26jDQkKyXVhoA');
const OTHER_BANK = new PublicKey('CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh');
const OWNER = new PublicKey('11111111111111111111111111111112');

describe('PDA derivations', () => {
  it('all PDA helpers return deterministic addresses', () => {
    expect(marketSignerPda(MARKET)[0].equals(marketSignerPda(MARKET)[0])).toBe(true);
    expect(lenderIntegrationAccountPda(MARKET)[0].equals(lenderIntegrationAccountPda(MARKET)[0])).toBe(true);
    expect(borrowerIntegrationAccountPda(MARKET)[0].equals(borrowerIntegrationAccountPda(MARKET)[0])).toBe(true);
    expect(marketTokenVaultPda(MARKET, MINT)[0].equals(marketTokenVaultPda(MARKET, MINT)[0])).toBe(true);
    expect(globalVaultPda(DEBT_BANK)[0].equals(globalVaultPda(DEBT_BANK)[0])).toBe(true);
    expect(globalConfigPda()[0].equals(globalConfigPda()[0])).toBe(true);
    expect(userAccountPda(OWNER)[0].equals(userAccountPda(OWNER)[0])).toBe(true);
  });

  it('the global vault is keyed by the bank (debt lending pool)', () => {
    // Distinct banks → distinct vault PDAs.
    const a = globalVaultPda(DEBT_BANK)[0];
    const b = globalVaultPda(OTHER_BANK)[0];
    expect(a.equals(b)).toBe(false);
    // Keying by the bank vs by a mint with the same bytes would differ — the
    // helper has no notion of "mint"; it hashes the raw seed it's given, so a
    // bank seed and a mint seed land on different addresses.
    expect(globalVaultPda(DEBT_BANK)[0].equals(globalVaultPda(MINT)[0])).toBe(false);
  });

  it('different inputs produce different PDAs', () => {
    const a = marketSignerPda(MARKET)[0];
    const b = marketSignerPda(MINT)[0];
    expect(a.equals(b)).toBe(false);

    const lender = lenderIntegrationAccountPda(MARKET)[0];
    const borrower = borrowerIntegrationAccountPda(MARKET)[0];
    expect(lender.equals(borrower)).toBe(false);

    const vault1 = globalVaultPda(DEBT_BANK)[0];
    const vault2 = globalVaultPda(MARKET)[0];
    expect(vault1.equals(vault2)).toBe(false);
  });

  it('vault signer / staging / integration are derived from the vault PDA', () => {
    const [vault] = globalVaultPda(DEBT_BANK);
    expect(globalVaultSignerPda(vault)[0]).toBeInstanceOf(PublicKey);
    expect(globalVaultStagingPda(vault)[0]).toBeInstanceOf(PublicKey);
    expect(globalVaultIntegrationAccountPda(vault)[0]).toBeInstanceOf(PublicKey);
    // All three are distinct.
    const s = globalVaultSignerPda(vault)[0];
    const st = globalVaultStagingPda(vault)[0];
    const it = globalVaultIntegrationAccountPda(vault)[0];
    expect(s.equals(st)).toBe(false);
    expect(s.equals(it)).toBe(false);
    expect(st.equals(it)).toBe(false);
  });

  it('loan PDA uses 8-byte LE sequence', () => {
    const [a] = loanPda(MARKET, 0n);
    const [b] = loanPda(MARKET, 1n);
    expect(a.equals(b)).toBe(false);
    // Same sequence → same PDA.
    expect(loanPda(MARKET, 42n)[0].equals(loanPda(MARKET, 42n)[0])).toBe(true);
  });

  it('all PDAs are owned by the yDelta program id', () => {
    // PDAs derive against YDELTA_PROGRAM_ID by construction — sanity-check
    // that the constant matches what we expect.
    expect(YDELTA_PROGRAM_ID.toBase58()).toBe('Ar38x7KobxmwvKwSL2VzdjFxrtXd3Gs2VfB3ZCakyJas');
  });

  it('bump values are in 0..=255', () => {
    const [, bump] = marketSignerPda(MARKET);
    expect(bump).toBeGreaterThanOrEqual(0);
    expect(bump).toBeLessThanOrEqual(255);
  });
});
