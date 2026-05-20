/**
 * Tier 14 e2e: decode the `BadDebtLog` event emitted by an under-
 * collateralised liquidation. Verifies the on-chain bad-debt accounting
 * actually emits the right atoms, not just a hand-wavy log.
 *
 * Layout (from `programs/ydelta/src/logs.rs::BadDebtLog`):
 *   disc(8) || market(32) || loan(32) || gap_collateral_atoms(u64)
 *           || debt_atoms_remaining(u64) || _padding(16)
 * Total = 8 + 96 = 104 bytes.
 *
 * We don't compute the keccak-derived discriminator in TS — instead we
 * filter `meta.logMessages` to `Program data:` entries with exactly
 * 104 decoded bytes. In this spec the only event of that size emitted is
 * `BadDebtLog`.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  cuBudgetIx,
  globalVaultIntegrationAccountPda,
  globalVaultPda,
  globalVaultSignerPda,
  globalVaultStagingPda,
  HEAVY_IX_CU_LIMIT,
  lenderIntegrationAccountPda,
  liquidateLoanInstruction,
  loanPda,
  marketSignerPda,
  marketTokenVaultPda,
  processMatchedLoanInstruction,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import {
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  SOL_BANK,
  SOL_LIQUIDITY_VAULT,
  SOL_ORACLE,
  SPL_TOKEN_PROGRAM_ID,
  USDC_BANK,
  USDC_LIQUIDITY_VAULT,
  USDC_MINT,
  USDC_ORACLE,
  WSOL_MINT,
} from './_fixtures.ts';
import { bankLiquidityVaultAuthority, driveToMatchLanded, MatchLandedHandles } from './_setup.ts';

interface BadDebt {
  market: PublicKey;
  loan: PublicKey;
  gapCollateralAtoms: bigint;
  debtAtomsRemaining: bigint;
}

/** Scan tx logs for the lone `Program data:` entry of size 104 bytes. */
function findBadDebtLog(logMessages: string[]): BadDebt | null {
  for (const line of logMessages) {
    const prefix = 'Program data: ';
    const idx = line.indexOf(prefix);
    if (idx === -1) continue;
    const b64 = line.slice(idx + prefix.length).trim();
    const buf = Buffer.from(b64, 'base64');
    if (buf.length !== 104) continue;
    // Body starts at offset 8 (post-discriminator).
    const market = new PublicKey(buf.subarray(8, 40));
    const loan = new PublicKey(buf.subarray(40, 72));
    const gapCollateralAtoms = buf.readBigUInt64LE(72);
    const debtAtomsRemaining = buf.readBigUInt64LE(80);
    return { market, loan, gapCollateralAtoms, debtAtomsRemaining };
  }
  return null;
}

describe('e2e: BadDebtLog emitted by under-collateralised liquidation', () => {
  let bk: BankrunHandle;
  let handles: MatchLandedHandles;
  let cranker: Keypair;
  let keeper: Keypair;
  let keeperUsdcAta: PublicKey;
  let keeperSolAta: PublicKey;
  let loanKey: PublicKey;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    handles = await driveToMatchLanded(bk, {
      principalAtoms: 1_000_000n,
      collateralAtoms: 50_000_000n,
      vaultDepositAtoms: 1_000_000_000n,
      wsolFundAtoms: 200_000_000n,
      collateralDepositAtoms: 100_000_000n,
    });
    cranker = await bk.fundedKeypair();
    loanKey = loanPda(handles.market.publicKey, handles.matchedLoanSequence)[0];

    const vault = globalVaultPda(USDC_MINT)[0];
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE });
    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        processMatchedLoanInstruction({
          payer: cranker.publicKey,
          market: handles.market.publicKey,
          debtBank: USDC_BANK,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          sequence: handles.matchedLoanSequence,
          vaultSettle: {
            globalVault: vault,
            globalVaultSigner: globalVaultSignerPda(vault)[0],
            globalVaultStaging: globalVaultStagingPda(vault)[0],
            globalVaultIntegrationAccount: globalVaultIntegrationAccountPda(vault)[0],
            marketDebtVault: marketTokenVaultPda(handles.market.publicKey, USDC_MINT)[0],
            marketLenderIntegrationAccount: lenderIntegrationAccountPda(handles.market.publicKey)[0],
            marketSigner: marketSignerPda(handles.market.publicKey)[0],
            debtLiquidityVault: USDC_LIQUIDITY_VAULT,
            debtBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(USDC_BANK),
            debtOracles: [USDC_ORACLE],
            debtMint: USDC_MINT,
            tokenProgram: SPL_TOKEN_PROGRAM_ID,
            marginfiGroup: MARGINFI_GROUP,
            marginfiProgram: MARGINFI_PROGRAM_ID,
          },
        }),
      ],
      [cranker],
    );

    keeper = await bk.fundedKeypair();
    keeperUsdcAta = Keypair.generate().publicKey;
    keeperSolAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: keeperUsdcAta,
      mint: USDC_MINT,
      owner: keeper.publicKey,
      amount: 10_000_000_000n,
    });
    await bk.putWsolTokenAccount({
      address: keeperSolAta,
      owner: keeper.publicKey,
      amount: 0n,
    });

    // Crash wSOL → $0.001 to force the under-collateralised branch.
    await bk.setSwbOraclePriceAtoms(SOL_ORACLE, 1_000_000_000_000_000n);
  });

  it('Under-collateralised liquidate emits BadDebtLog with gap > 0 + debt_remaining = 0', async () => {
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    const meta = await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        liquidateLoanInstruction({
          payer: keeper.publicKey,
          market: handles.market.publicKey,
          sequence: handles.matchedLoanSequence,
          debtMint: USDC_MINT,
          collateralMint: WSOL_MINT,
          liquidatorDebtToken: keeperUsdcAta,
          liquidatorCollateralToken: keeperSolAta,
          debtBank: USDC_BANK,
          collateralBank: SOL_BANK,
          debtLiquidityVault: USDC_LIQUIDITY_VAULT,
          collateralLiquidityVault: SOL_LIQUIDITY_VAULT,
          collateralBankLiquidityVaultAuthority: bankLiquidityVaultAuthority(SOL_BANK),
          debtOracles: [USDC_ORACLE],
          collateralOracles: [SOL_ORACLE],
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          repayAtomsMax: 0n, // full repay attempt
          crankerRefund: cranker.publicKey,
        }),
      ],
      [keeper],
    );

    const badDebt = findBadDebtLog(meta.logMessages);
    expect(badDebt).not.toBeNull();
    expect(badDebt!.market.equals(handles.market.publicKey)).toBe(true);
    expect(badDebt!.loan.equals(loanKey)).toBe(true);
    // gap_collateral_atoms > 0 — the under-collateralised condition.
    expect(badDebt!.gapCollateralAtoms).toBeGreaterThan(0n);
    // For a FULL liquidate attempt that hits the bad-debt branch:
    // debt_atoms_remaining = 0 — the keeper paid the full outstanding
    // amount, but seized only the available collateral; the gap is the
    // shortfall in collateral value vs the debt that was retired.
    expect(badDebt!.debtAtomsRemaining).toBe(0n);
  });
});
