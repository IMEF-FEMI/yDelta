/**
 * Tier 3c e2e: full match flow.
 *
 * Sequence (everything a real UX user would do, end-to-end):
 *   1. Global config + market + vault + risk profile (`max_ltv = 80%`).
 *   2. Curator: GlobalVaultDeposit 100 USDC → profile idle.
 *   3. Curator: PlaceOrderForRiskProfile at 500 bps / 30 days.
 *   4. Borrower: ClaimSeat + Deposit wSOL collateral.
 *   5. Borrower: PlaceOrder IOC bid at 800 bps / 30 days.
 *   6. Verify: match landed (encumbered_in_orders_atoms grew, matched_loan
 *      node in the queue, ask still resting — unbounded quote semantics).
 *
 * Amounts and ratios mirror the Rust `vault_ask_crossed_by_borrower_bid_full_fill`
 * integration test — tiny atom amounts so the LTV math is trivially under the
 * 80% cap regardless of the mainnet oracle prices baked into the fixtures.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  claimSeatInstruction,
  cuBudgetIx,
  decodeGlobalVault,
  decodeMarket,
  depositInstruction,
  globalVaultDepositInstruction,
  globalVaultPda,
  placeOrderForRiskProfileInstruction,
  placeOrderInstruction,
  HEAVY_IX_CU_LIMIT,
  MATCHED_LOAN_FLAG_VAULT_LENDER,
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
import { setupGlobalConfig, setupMarket, setupRiskProfile, setupVault } from './_setup.ts';

/**
 * Derive a marginfi v0.1.8 bank's `liquidity_vault_authority` PDA. Seeds:
 * `[b"liquidity_vault_auth", bank_pubkey]` under the marginfi program id.
 */
function bankLiquidityVaultAuthority(bank: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('liquidity_vault_auth'), bank.toBuffer()],
    MARGINFI_PROGRAM_ID,
  )[0];
}

// Match the Rust integration-test recipe so the LTV gate doesn't reject the
// cross at the dumped mainnet oracle prices. Tiny atoms = trivially-OK LTV.
const VAULT_DEPOSIT_ATOMS = 100_000_000n; // 100 USDC
const ASK_RATE_BPS = 500;
const BID_RATE_BPS = 800;
const TERM_SECONDS = 30 * 86_400;
const MAX_LTV_BPS = 8_000;
const WSOL_FUND_ATOMS = 100_000n; // 0.0001 SOL — well over the 5_000-collateral bid
const COLLATERAL_DEPOSIT_ATOMS = 10_000n; // 0.00001 SOL onto the seat
const BID_PRINCIPAL_ATOMS = 100n; // tiny — keeps LTV math far below cap
const BID_COLLATERAL_ATOMS = 5_000n;

describe('e2e: borrower IOC bid crosses curator vault ask', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let curator: Keypair;
  let depositor: Keypair;
  let depositorUsdcAta: PublicKey;
  let borrower: Keypair;
  let borrowerSolAta: PublicKey;
  let borrowerUsdcAta: PublicKey;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;

    // Global config + market + vault + risk profile.
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await setupVault(bk, admin);
    curator = await bk.fundedKeypair();
    await setupRiskProfile(bk, admin, curator.publicKey, {
      maxLtvBps: MAX_LTV_BPS,
      maxTermSeconds: TERM_SECONDS,
    });

    // Depositor funds the profile.
    depositor = await bk.fundedKeypair();
    depositorUsdcAta = Keypair.generate().publicKey;
    await bk.putTokenAccount({
      address: depositorUsdcAta,
      mint: USDC_MINT,
      owner: depositor.publicKey,
      amount: VAULT_DEPOSIT_ATOMS * 2n,
    });
    await bk.send(
      [
        globalVaultDepositInstruction({
          depositor: depositor.publicKey,
          mint: USDC_MINT,
          depositorToken: depositorUsdcAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          lendingPool: USDC_BANK,
          liquidityVault: USDC_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          profileId: 0,
          amountAtoms: VAULT_DEPOSIT_ATOMS,
        }),
      ],
      [depositor],
    );

    // Curator quotes an unbounded ask.
    await bk.send(
      [
        placeOrderForRiskProfileInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          mint: USDC_MINT,
          market: market.publicKey,
          profileId: 0,
          rateBps: ASK_RATE_BPS,
          termSeconds: TERM_SECONDS,
        }),
      ],
      [curator],
    );

    // Borrower claims a seat + deposits collateral.
    borrower = await bk.fundedKeypair();
    borrowerSolAta = Keypair.generate().publicKey;
    borrowerUsdcAta = Keypair.generate().publicKey;
    await bk.putWsolTokenAccount({
      address: borrowerSolAta,
      owner: borrower.publicKey,
      amount: WSOL_FUND_ATOMS,
    });
    await bk.putTokenAccount({
      address: borrowerUsdcAta,
      mint: USDC_MINT,
      owner: borrower.publicKey,
      amount: 0n,
    });
    await bk.send([claimSeatInstruction({ payer: borrower.publicKey, market: market.publicKey })], [borrower]);
    await bk.send(
      [
        depositInstruction({
          payer: borrower.publicKey,
          market: market.publicKey,
          mint: WSOL_MINT,
          debtMint: USDC_MINT,
          traderToken: borrowerSolAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          bank: SOL_BANK,
          liquidityVault: SOL_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          amountAtoms: COLLATERAL_DEPOSIT_ATOMS,
        }),
      ],
      [borrower],
    );
  });

  it('pre-cross profile is fully idle (zero encumbered / deployed)', async () => {
    const [vaultPda] = globalVaultPda(USDC_MINT);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    const profile = vault.riskProfiles[0].profile;
    expect(profile.encumberedInOrdersAtoms).toBe(0n);
    expect(profile.deployedPrincipalAtoms).toBe(0n);
    // Credited principal is the marginfi-ACKNOWLEDGED deposit (gross minus
    // sub-atom share-rounding on the deposit CPI), so allow a couple atoms
    // below the gross.
    expect(profile.totalPrincipalAtoms).toBeLessThanOrEqual(VAULT_DEPOSIT_ATOMS);
    expect(profile.totalPrincipalAtoms).toBeGreaterThan(VAULT_DEPOSIT_ATOMS - 4n);
  });

  it('PlaceOrder crosses the vault ask, bumps encumbered + queues a MatchedLoan', async () => {
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    const debtBankLva = bankLiquidityVaultAuthority(USDC_BANK);

    await bk.send(
      [
        cuBudgetIx(HEAVY_IX_CU_LIMIT),
        placeOrderInstruction({
          payer: borrower.publicKey,
          market: market.publicKey,
          debtMint: USDC_MINT,
          marginfiGroup: MARGINFI_GROUP,
          debtBank: USDC_BANK,
          collateralBank: SOL_BANK,
          debtOracles: [USDC_ORACLE],
          collateralOracles: [SOL_ORACLE],
          debtLiquidityVault: USDC_LIQUIDITY_VAULT,
          debtBankLiquidityVaultAuthority: debtBankLva,
          borrowerDebtToken: borrowerUsdcAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          rateBps: BID_RATE_BPS,
          termSeconds: TERM_SECONDS,
          principalAtoms: BID_PRINCIPAL_ATOMS,
          collateralAtoms: BID_COLLATERAL_ATOMS,
          // flags = 0 → P2Pool fallback on. Default-args path the SDK exposes.
        }),
      ],
      [borrower],
    );

    // Vault profile state: match-time bookkeeping bumps encumbered, NOT
    // deployed. Atoms physically stay in the vault's marginfi integration
    // account until the cranker promotes the queue node to a real Loan PDA.
    const [vaultPda] = globalVaultPda(USDC_MINT);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    const profile = vault.riskProfiles[0].profile;
    expect(profile.encumberedInOrdersAtoms).toBe(BID_PRINCIPAL_ATOMS);
    expect(profile.deployedPrincipalAtoms).toBe(0n);
    // Marginfi-acknowledged deposit (gross minus sub-atom share-rounding).
    expect(profile.totalPrincipalAtoms).toBeLessThanOrEqual(VAULT_DEPOSIT_ATOMS);
    expect(profile.totalPrincipalAtoms).toBeGreaterThan(VAULT_DEPOSIT_ATOMS - 4n);

    // Market: one MatchedLoan queue node landed. The ask is unbounded so
    // it stays in the asks tree.
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(m.matchedLoans).toHaveLength(1);
    const ml = m.matchedLoans[0].loan;
    expect(ml.principalAtoms).toBe(BID_PRINCIPAL_ATOMS);
    expect(ml.collateralAtoms).toBe(BID_COLLATERAL_ATOMS);
    expect(ml.lenderRateBps).toBe(ASK_RATE_BPS); // borrower locks in the ask rate
    expect(ml.borrowerRateBps).toBeGreaterThanOrEqual(ASK_RATE_BPS); // ≥ ask + protocol fee floor
    expect(ml.termSeconds).toBe(TERM_SECONDS);
    expect(ml.flags & MATCHED_LOAN_FLAG_VAULT_LENDER).toBe(MATCHED_LOAN_FLAG_VAULT_LENDER);

    // Borrower seat: collateral encumbrance happens at `process_matched_loan`
    // (cranker) time, not at match time — the matched-loan queue node carries
    // the seat indices but the borrower's `collateral_withdrawable → encumbered`
    // shift is performed when the cranker promotes the queue node to a
    // `LoanFixed` PDA. So at this point the seat still shows the full
    // deposit as collateral_withdrawable, and BOTH debt-side fields are 0
    // (no debt has been credited to the seat yet — the cranker does that).
    const borrowerSeat = m.claimedSeats.find((s) => s.seat.owner.equals(borrower.publicKey))!.seat;
    expect(borrowerSeat.collateralWithdrawableShares).toBeGreaterThan(0n);
    expect(borrowerSeat.collateralEncumberedShares).toBe(0n);
    expect(borrowerSeat.debtWithdrawableShares).toBe(0n);
    expect(borrowerSeat.debtEncumberedShares).toBe(0n);

    // The vault ask itself remains on the book — unbounded "quote-all-idle".
    expect(m.asks).toHaveLength(1);
    expect(m.asks[0].order.rateBps).toBe(ASK_RATE_BPS);
    expect(m.asks[0].order.termSeconds).toBe(TERM_SECONDS);
    // Profile is the ONLY one in the vault and the seat invariant holds:
    // post-cross profile state is what the match-engine writes — no
    // collateral or debt counters touched until the cranker promotes.
    expect(vault.depositorSeats).toHaveLength(1); // the original depositor
  });
});
