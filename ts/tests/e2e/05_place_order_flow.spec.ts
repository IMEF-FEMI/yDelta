/**
 * Tier 3c e2e: full match flow.
 *
 * Sequence (everything a real UX user would do, end-to-end):
 *   1. Global config + market + vault + sub-vault (`max_ltv = 80%`).
 *   2. Curator: GlobalVaultDeposit 100 USDC → sub-vault idle.
 *   3. Curator: PlaceOrderForSubVault — rate quoted live (bank APR + spread).
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
  placeOrderForSubVaultInstruction,
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
import { setupGlobalConfig, setupMarket, setupPoolSubVault, setupVault } from './_setup.ts';

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
  // v1: the vault ask rate is quoted live by the program (bank lending APR +
  // sub_vault.spread_bps), read back after placement. The bid is set above it.
  let askRateBps: number;
  let bidRateBps: number;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;

    // Global config + market + vault + sub-vault.
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);
    await setupVault(bk, admin);
    curator = await bk.fundedKeypair();
    await setupPoolSubVault(bk, admin, curator.publicKey, {
      maxLtvBps: MAX_LTV_BPS,
      maxTermSeconds: TERM_SECONDS,
    });

    // Depositor funds the subVault.
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
          subVaultId: 1,
          amountAtoms: VAULT_DEPOSIT_ATOMS,
        }),
      ],
      [depositor],
    );

    // Curator rests an unbounded ask. v1: no rate/term args — the program
    // quotes the rate live (bank lending APR + spread) and uses the
    // sub-vault's max_term_seconds, so it reads the bank + oracles.
    await bk.refreshOracleFreshness({ pythOracle: USDC_ORACLE, switchboardOracle: SOL_ORACLE });
    await bk.send(
      [
        placeOrderForSubVaultInstruction({
          feePayer: curator.publicKey,
          curator: curator.publicKey,
          market: market.publicKey,
          debtBank: USDC_BANK,
          marginfiGroup: MARGINFI_GROUP,
          collateralBank: SOL_BANK,
          debtOracles: [USDC_ORACLE],
          collateralOracles: [SOL_ORACLE],
          subVaultId: 1,
        }),
      ],
      [curator],
    );

    // Read back the program-quoted ask rate; the borrower bids above it.
    const askMarket = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    askRateBps = askMarket.asks[0].order.rateBps;
    bidRateBps = askRateBps + 300;

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

  it('pre-cross sub-vault is fully idle (zero encumbered / deployed)', async () => {
    const [vaultPda] = globalVaultPda(USDC_BANK);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    const subVault = vault.subVaults[0].subVault;
    expect(subVault.encumberedInOrdersAtoms).toBe(0n);
    expect(subVault.deployedPrincipalAtoms).toBe(0n);
    // Credited principal is the marginfi-ACKNOWLEDGED deposit (gross minus
    // sub-atom share-rounding on the deposit CPI), so allow a couple atoms
    // below the gross.
    expect(subVault.totalPrincipalAtoms).toBeLessThanOrEqual(VAULT_DEPOSIT_ATOMS);
    expect(subVault.totalPrincipalAtoms).toBeGreaterThan(VAULT_DEPOSIT_ATOMS - 4n);
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
          rateBps: bidRateBps,
          termSeconds: TERM_SECONDS,
          principalAtoms: BID_PRINCIPAL_ATOMS,
          collateralAtoms: BID_COLLATERAL_ATOMS,
          // residualMode defaults to 0 → P2Pool fallback on (v1 D6).
        }),
      ],
      [borrower],
    );

    // Vault subVault state: match-time bookkeeping bumps encumbered, NOT
    // deployed. Atoms physically stay in the vault's marginfi integration
    // account until the cranker promotes the queue node to a real Loan PDA.
    const [vaultPda] = globalVaultPda(USDC_BANK);
    const vault = decodeGlobalVault((await bk.getAccount(vaultPda))!.data);
    const subVault = vault.subVaults[0].subVault;
    expect(subVault.encumberedInOrdersAtoms).toBe(BID_PRINCIPAL_ATOMS);
    expect(subVault.deployedPrincipalAtoms).toBe(0n);
    // Marginfi-acknowledged deposit (gross minus sub-atom share-rounding).
    expect(subVault.totalPrincipalAtoms).toBeLessThanOrEqual(VAULT_DEPOSIT_ATOMS);
    expect(subVault.totalPrincipalAtoms).toBeGreaterThan(VAULT_DEPOSIT_ATOMS - 4n);

    // Market: one MatchedLoan queue node landed. The ask is unbounded so
    // it stays in the asks tree.
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(m.matchedLoans).toHaveLength(1);
    const ml = m.matchedLoans[0].loan;
    expect(ml.principalAtoms).toBe(BID_PRINCIPAL_ATOMS);
    expect(ml.collateralAtoms).toBe(BID_COLLATERAL_ATOMS);
    expect(ml.lenderRateBps).toBe(askRateBps); // borrower locks in the ask rate
    expect(ml.borrowerRateBps).toBeGreaterThanOrEqual(askRateBps); // ≥ ask + protocol fee floor
    expect(ml.termSeconds).toBe(TERM_SECONDS);
    expect(ml.flags & MATCHED_LOAN_FLAG_VAULT_LENDER).toBe(MATCHED_LOAN_FLAG_VAULT_LENDER);

    // Borrower seat: v1 `place_order` encumbers the bid's collateral up front
    // (`encumber_for_order(Side::Bid)` shifts collateral_withdrawable →
    // collateral_encumbered) BEFORE the match runs; the matched portion stays
    // encumbered to back the queued MatchedLoan. So post-cross the seat shows
    // the bid collateral as ENCUMBERED, with the unbid remainder of the
    // deposit still withdrawable. The debt-side fields stay 0 — no debt is
    // credited to the borrower seat until the cranker promotes the queue node
    // to a `LoanFixed` PDA.
    const borrowerSeat = m.claimedSeats.find((s) => s.seat.owner.equals(borrower.publicKey))!.seat;
    expect(borrowerSeat.collateralEncumberedShares).toBeGreaterThan(0n);
    expect(borrowerSeat.collateralWithdrawableShares).toBeGreaterThan(0n);
    expect(borrowerSeat.debtWithdrawableShares).toBe(0n);
    expect(borrowerSeat.debtEncumberedShares).toBe(0n);

    // The vault ask itself remains on the book — unbounded "quote-all-idle".
    expect(m.asks).toHaveLength(1);
    expect(m.asks[0].order.rateBps).toBe(askRateBps);
    expect(m.asks[0].order.termSeconds).toBe(TERM_SECONDS);
    // SubVault is the ONLY one in the vault and the seat invariant holds:
    // post-cross subVault state is what the match-engine writes — no
    // collateral or debt counters touched until the cranker promotes.
    expect(vault.depositorSeats).toHaveLength(1); // the original depositor
  });
});
