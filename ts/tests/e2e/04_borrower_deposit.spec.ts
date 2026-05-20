/**
 * Tier 3b e2e: borrower-side setup.
 *
 *   1. ClaimSeat — borrower allocates a user-owned ClaimedSeat in the market.
 *   2. Deposit (collateral side) — SOL hops from a synth wallet → market
 *      collateral vault → borrower marginfi account via marginfi.deposit
 *      CPI. Borrower's seat's `collateral_withdrawable_shares` grows.
 *
 * No oracle refresh required — `deposit` does not invoke marginfi's
 * health check. `place_order` (Tier 3c) is where the oracle dance kicks in.
 */
import { beforeAll, describe, expect, it } from 'vitest';
import { Keypair, PublicKey } from '@solana/web3.js';

import {
  claimSeatInstruction,
  decodeBank,
  decodeMarket,
  depositInstruction,
  FP48_SHIFT,
  OwnerKind,
} from '../../src/index.js';
import { bootBankrun, BankrunHandle } from './_bankrun.ts';
import {
  MARGINFI_GROUP,
  MARGINFI_PROGRAM_ID,
  SOL_BANK,
  SOL_LIQUIDITY_VAULT,
  SPL_TOKEN_PROGRAM_ID,
  USDC_MINT,
  WSOL_MINT,
} from './_fixtures.ts';
import { setupGlobalConfig, setupMarket } from './_setup.ts';

describe('e2e: borrower seat + collateral deposit', () => {
  let bk: BankrunHandle;
  let admin: Keypair;
  let market: Keypair;
  let borrower: Keypair;
  let borrowerSolAta: PublicKey;

  beforeAll(async () => {
    bk = await bootBankrun({ loadMarginfiFixtures: true });
    admin = bk.payer;
    await setupGlobalConfig(bk, admin);
    market = await setupMarket(bk, admin);

    borrower = await bk.fundedKeypair();
    // Synth wSOL token account holding 5 SOL. wSOL needs `is_native=Some(rent)`
    // and `lamports = rent_exempt + amount` because SPL Token's native-transfer
    // logic moves lamports alongside the `amount` field — `putTokenAccount`
    // is for non-native mints only.
    borrowerSolAta = Keypair.generate().publicKey;
    await bk.putWsolTokenAccount({
      address: borrowerSolAta,
      owner: borrower.publicKey,
      amount: 5_000_000_000n,
    });
  });

  it('ClaimSeat allocates a user-owned ClaimedSeat in the market', async () => {
    await bk.send([claimSeatInstruction({ payer: borrower.publicKey, market: market.publicKey })], [borrower]);
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    expect(m.claimedSeats).toHaveLength(1);
    const seat = m.claimedSeats[0].seat;
    expect(seat.owner.equals(borrower.publicKey)).toBe(true);
    expect(seat.ownerKind).toBe(OwnerKind.User);
    expect(seat.collateralWithdrawableShares).toBe(0n);
  });

  it('Deposit (collateral side) credits borrower seat with collateral shares', async () => {
    await bk.send(
      [
        depositInstruction({
          payer: borrower.publicKey,
          market: market.publicKey,
          mint: WSOL_MINT, // collateral side — bank picks borrower marginfi acct
          debtMint: USDC_MINT,
          traderToken: borrowerSolAta,
          tokenProgram: SPL_TOKEN_PROGRAM_ID,
          marginfiGroup: MARGINFI_GROUP,
          bank: SOL_BANK,
          liquidityVault: SOL_LIQUIDITY_VAULT,
          marginfiProgram: MARGINFI_PROGRAM_ID,
          amountAtoms: 3_000_000_000n, // 3 SOL
        }),
      ],
      [borrower],
    );

    // Borrower seat: the deposit credits exactly `amountAtoms` worth of
    // collateral shares. Back-compute `(shares × asset_share_value) >> 96`
    // and bound against the deposit (±1 atom share-round dust).
    const depositAtoms = 3_000_000_000n;
    const m = decodeMarket((await bk.getAccount(market.publicKey))!.data);
    const seat = m.claimedSeats.find((s) => s.seat.owner.equals(borrower.publicKey))!.seat;
    const bank = decodeBank((await bk.getAccount(SOL_BANK))!.data);
    const collateralAtomsBack =
      (seat.collateralWithdrawableShares * bank.assetShareValueFp48) >> (FP48_SHIFT * 2n);
    const drift =
      collateralAtomsBack > depositAtoms ? collateralAtomsBack - depositAtoms : depositAtoms - collateralAtomsBack;
    expect(drift).toBeLessThanOrEqual(1n);
    expect(seat.collateralEncumberedShares).toBe(0n);
    expect(seat.debtWithdrawableShares).toBe(0n);
    expect(seat.debtEncumberedShares).toBe(0n);
  });
});
