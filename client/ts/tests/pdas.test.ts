import { expect } from 'chai';
import { PublicKey } from '@solana/web3.js';
import {
  YDELTA_PROGRAM_ID,
  globalConfigPda,
  marketSignerPda,
  lenderMarginfiAccountPda,
  borrowerMarginfiAccountPda,
  marketVaultPda,
  loanPda,
  splitLoanPda,
  userAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  vaultIntegrationAccountPda,
  vaultStagingPda,
} from '../src/utils';

const FIXED_MARKET = new PublicKey('11111111111111111111111111111112');
const FIXED_MINT = new PublicKey('11111111111111111111111111111114');
const FIXED_OWNER = new PublicKey('11111111111111111111111111111113');

describe('PDA derivations', () => {
  it('exposes the canonical program id', () => {
    expect(YDELTA_PROGRAM_ID.toBase58()).to.equal(
      '9Tcnk3xQKXeoSdY7ovTyGtGGFbBxraQR7TDhRE2UyXRT',
    );
  });

  it('derives GlobalConfig as a singleton', () => {
    const [a, ba] = globalConfigPda();
    const [b] = globalConfigPda();
    expect(a.equals(b)).to.equal(true);
    expect(typeof ba).to.equal('number');
  });

  it('derives MarketSigner from market key', () => {
    const [signer] = marketSignerPda(FIXED_MARKET);
    expect(PublicKey.isOnCurve(signer.toBytes())).to.equal(false);
  });

  it('lender and borrower marginfi PDAs are distinct for the same market', () => {
    const [lender] = lenderMarginfiAccountPda(FIXED_MARKET);
    const [borrower] = borrowerMarginfiAccountPda(FIXED_MARKET);
    expect(lender.equals(borrower)).to.equal(false);
  });

  it('market staging vault uses 3-seed shape and differs from global vault', () => {
    const [staging] = marketVaultPda(FIXED_MARKET, FIXED_MINT);
    const [global] = globalVaultPda(FIXED_MINT);
    expect(staging.equals(global)).to.equal(false);
  });

  it('loan PDA varies with sequence', () => {
    const [a] = loanPda(FIXED_MARKET, 1n);
    const [b] = loanPda(FIXED_MARKET, 2n);
    expect(a.equals(b)).to.equal(false);
  });

  it('split loan PDA uses a distinct seed prefix from primary loans', () => {
    const [primary] = loanPda(FIXED_MARKET, 7n);
    const [split] = splitLoanPda(FIXED_MARKET, 7n);
    expect(primary.equals(split)).to.equal(false);
  });

  it('user account PDA is deterministic per owner', () => {
    const [a] = userAccountPda(FIXED_OWNER);
    const [b] = userAccountPda(FIXED_OWNER);
    expect(a.equals(b)).to.equal(true);
  });

  it('vault signer / integration / staging are all distinct', () => {
    const [vault] = globalVaultPda(FIXED_MINT);
    const [signer] = globalVaultSignerPda(vault);
    const [integration] = vaultIntegrationAccountPda(vault);
    const [staging] = vaultStagingPda(vault);
    const set = new Set([vault.toBase58(), signer.toBase58(), integration.toBase58(), staging.toBase58()]);
    expect(set.size).to.equal(4);
  });
});
