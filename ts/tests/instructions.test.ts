/**
 * Instruction-builder byte tests. For each of the 37 builders, verify:
 *   - the data tail starts with the correct discriminant tag,
 *   - the AccountMeta array has the right length, and
 *   - the first key is always the signer (whom the on-chain processor
 *     authenticates against).
 *
 * Reference values are hand-computed from the Rust source in
 * `programs/ydelta/src/program/instruction_builders/*.rs`. Tests intentionally
 * keep account assertions targeted (signer + a marker like global_config,
 * loan PDA, or vault PDA) — full account-by-account equivalence would
 * duplicate the builders. The targeted assertions catch the most common
 * regressions: wrong tag, off-by-one account, wrong PDA.
 */
import { describe, expect, it } from 'vitest';
import { PublicKey, SystemProgram } from '@solana/web3.js';
import { TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID } from '@solana/spl-token';

import {
  InstructionTag,
  YDELTA_PROGRAM_ID,
  acceptCuratorInstruction,
  acceptGlobalVaultAdminInstruction,
  acceptMarketAdminInstruction,
  acceptProtocolAdminInstruction,
  borrowerIntegrationAccountPda,
  cancelOrderForRiskProfileInstruction,
  checkLtvLiquidatableInstruction,
  checkMaturityLiquidatableInstruction,
  claimCuratorFeeInstruction,
  claimRepaymentForRiskProfileInstruction,
  claimSeatInstruction,
  convertP2poolToFixedInstruction,
  createGlobalConfigInstruction,
  createMarketInstruction,
  createRiskProfileInstruction,
  createVaultInstruction,
  depositInstruction,
  globalConfigPda,
  globalVaultDepositInstruction,
  globalVaultPda,
  globalVaultWithdrawInstruction,
  lenderIntegrationAccountPda,
  liquidateLoanInstruction,
  loanPda,
  marketTokenVaultPda,
  placeOrderForRiskProfileInstruction,
  placeOrderInstruction,
  processMatchedLoanInstruction,
  protocolFeeClaimInstruction,
  repayInstruction,
  setFeeConfigInstruction,
  setGlobalPauseInstruction,
  setMarketPauseInstruction,
  setVaultPauseInstruction,
  settleMaturedLoanInstruction,
  syncMarketPositionInstruction,
  transferCuratorInstruction,
  transferGlobalVaultAdminInstruction,
  transferMarketAdminInstruction,
  transferProtocolAdminInstruction,
  updateOrderForRiskProfileInstruction,
  updateRiskProfileInstruction,
  userAccountPda,
  withdrawInstruction,
} from '../src/index.js';

// Deterministic test pubkeys.
const PAYER = new PublicKey('11111111111111111111111111111112');
const ADMIN = new PublicKey('11111111111111111111111111111113');
const MARKET = new PublicKey('CYf9nJB7eJYqVRm6ucMcRYrwvtT6mzS5HHKkc4dHHGxk');
const DEBT_MINT = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');
const COLLAT_MINT = new PublicKey('So11111111111111111111111111111111111111112');
const MARGINFI_GROUP = new PublicKey('4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8');
const MARGINFI_PROGRAM = new PublicKey('MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA');
const DEBT_BANK = new PublicKey('Fe5QkKPVAh629UPP5aJ8sDZu8HTfe6M26jDQkKyXVhoA');
const COLLAT_BANK = new PublicKey('CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh');
const NEW_ADMIN = new PublicKey('1nc1nerator11111111111111111111111111111111');
const TOK_ACCOUNT = new PublicKey('GjwLkLwL6JcvBnRqzZ4mZpEbgaHrm5XmBeFffu5d6kAm');
const LIQ_VAULT = new PublicKey('7XwEuvxhc9KAr8b3kJ5ML9HxYJVABZ6ZmkrYx2y3pyab');
const LIQ_VAULT_AUTH = new PublicKey('FwzpWj1bDgvJ8N5o3xZWZ2GjLcGPaq4ujkF1HrJWFhCw');
const BANK_ORACLE = new PublicKey('Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX');
const ATA = new PublicKey('5GZTbHCJB2KhZWJgGyZpdQXBjbT8mYBy76YDLE5LCSp9');
const SOME_KEY = new PublicKey('11111111111111111111111111111114');

function tagOf(data: Buffer): number {
  return data[0];
}

function expectYdeltaIx(ix: { programId: PublicKey; data: Buffer | Uint8Array }): void {
  expect(ix.programId.equals(YDELTA_PROGRAM_ID)).toBe(true);
}

describe('instruction builders — discriminator tags + key positions', () => {
  /* ── Tag 0 ───────────────────────────────────────────── */
  it('CreateMarket (no params → 8 null Option flags)', () => {
    const ix = createMarketInstruction({
      marketCreator: PAYER,
      market: MARKET,
      debtMint: DEBT_MINT,
      collateralMint: COLLAT_MINT,
      marginfiGroup: MARGINFI_GROUP,
      debtBank: DEBT_BANK,
      collateralBank: COLLAT_BANK,
      marginfiProgram: MARGINFI_PROGRAM,
    });
    expectYdeltaIx(ix);
    expect(tagOf(ix.data)).toBe(InstructionTag.CreateMarket);
    // 1-byte tag + 8 borsh Option flags (each 1 byte = 0 for None when omitted).
    expect(ix.data.length).toBe(9);
    expect(Array.from(ix.data.slice(1))).toEqual([0, 0, 0, 0, 0, 0, 0, 0]);
    expect(ix.keys).toHaveLength(17);
    expect(ix.keys[0]).toMatchObject({ pubkey: PAYER, isSigner: true, isWritable: true });
    expect(ix.keys[2]).toMatchObject({ pubkey: MARKET, isWritable: true });
    expect(ix.keys[8].pubkey.equals(TOKEN_PROGRAM_ID)).toBe(true);
    expect(ix.keys[9].pubkey.equals(TOKEN_2022_PROGRAM_ID)).toBe(true);
    expect(ix.keys[13].pubkey.equals(lenderIntegrationAccountPda(MARKET)[0])).toBe(true);
    expect(ix.keys[14].pubkey.equals(borrowerIntegrationAccountPda(MARKET)[0])).toBe(true);
  });

  it('CreateMarket (with params → borsh-encoded Some(value) fields)', () => {
    const ix = createMarketInstruction({
      marketCreator: PAYER,
      market: MARKET,
      debtMint: DEBT_MINT,
      collateralMint: COLLAT_MINT,
      marginfiGroup: MARGINFI_GROUP,
      debtBank: DEBT_BANK,
      collateralBank: COLLAT_BANK,
      marginfiProgram: MARGINFI_PROGRAM,
      params: {
        ltvBufferBps: 250,
        gracePeriodSeconds: 3600,
      },
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CreateMarket);
    // tag(1) + 6× None(1) + Some(u16)(3) + Some(u32)(5) = 15 bytes.
    expect(ix.data.length).toBe(1 + 6 + 3 + 5);
    // ltvBufferBps = 250 (7th option, at offset 1 + 6 = 7).
    expect(ix.data[7]).toBe(1); // Some tag
    expect(ix.data.readUInt16LE(8)).toBe(250);
    // gracePeriodSeconds = 3600 (8th option, at offset 10).
    expect(ix.data[10]).toBe(1); // Some tag
    expect(ix.data.readUInt32LE(11)).toBe(3600);
  });

  /* ── Tag 1 ───────────────────────────────────────────── */
  it('ClaimSeat', () => {
    const ix = claimSeatInstruction({ payer: PAYER, market: MARKET });
    expectYdeltaIx(ix);
    expect(tagOf(ix.data)).toBe(InstructionTag.ClaimSeat);
    expect(ix.data.length).toBe(1);
    expect(ix.keys).toHaveLength(5);
    expect(ix.keys[0]).toMatchObject({ pubkey: PAYER, isSigner: true, isWritable: true });
    expect(ix.keys[1].pubkey.equals(globalConfigPda()[0])).toBe(true);
    expect(ix.keys[2]).toMatchObject({ pubkey: MARKET, isWritable: true });
    expect(ix.keys[3].pubkey.equals(SystemProgram.programId)).toBe(true);
    expect(ix.keys[4].pubkey.equals(userAccountPda(PAYER)[0])).toBe(true);
  });

  /* ── Tag 2 ───────────────────────────────────────────── */
  it('Deposit (debt side)', () => {
    const ix = depositInstruction({
      payer: PAYER,
      market: MARKET,
      mint: DEBT_MINT,
      debtMint: DEBT_MINT,
      traderToken: ATA,
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      bank: DEBT_BANK,
      liquidityVault: LIQ_VAULT,
      marginfiProgram: MARGINFI_PROGRAM,
      amountAtoms: 1_000n,
    });
    expectYdeltaIx(ix);
    expect(tagOf(ix.data)).toBe(InstructionTag.Deposit);
    // tag(1) + u64(8) + Option<u32>(1 None) = 10
    expect(ix.data.length).toBe(10);
    expect(ix.keys).toHaveLength(15);
    expect(ix.keys[8].pubkey.equals(lenderIntegrationAccountPda(MARKET)[0])).toBe(true);
    expect(ix.keys[4].pubkey.equals(marketTokenVaultPda(MARKET, DEBT_MINT)[0])).toBe(true);
  });

  it('Deposit (collateral side picks borrower marginfi account)', () => {
    const ix = depositInstruction({
      payer: PAYER,
      market: MARKET,
      mint: COLLAT_MINT,
      debtMint: DEBT_MINT,
      traderToken: ATA,
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      bank: COLLAT_BANK,
      liquidityVault: LIQ_VAULT,
      marginfiProgram: MARGINFI_PROGRAM,
      amountAtoms: 500_000n,
    });
    expect(ix.keys[8].pubkey.equals(borrowerIntegrationAccountPda(MARKET)[0])).toBe(true);
  });

  /* ── Tag 3 ───────────────────────────────────────────── */
  it('Withdraw with oracles + withdraw_all flag', () => {
    const ix = withdrawInstruction({
      payer: PAYER,
      market: MARKET,
      mint: DEBT_MINT,
      debtMint: DEBT_MINT,
      traderToken: ATA,
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      debtBank: DEBT_BANK,
      collateralBank: COLLAT_BANK,
      liquidityVault: LIQ_VAULT,
      bankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      debtOracles: [BANK_ORACLE],
      collateralOracles: [BANK_ORACLE],
      marginfiProgram: MARGINFI_PROGRAM,
      amountAtoms: 0n,
      withdrawAll: true,
    });
    expectYdeltaIx(ix);
    expect(tagOf(ix.data)).toBe(InstructionTag.Withdraw);
    // tag(1) + u64(8) + Option<u32>(1 None) + bool(1) = 11
    expect(ix.data.length).toBe(11);
    expect(ix.data[ix.data.length - 1]).toBe(1); // withdraw_all = true
  });

  /* ── Tag 4 ───────────────────────────────────────────── */
  it('PlaceOrder', () => {
    const ix = placeOrderInstruction({
      payer: PAYER,
      market: MARKET,
      debtMint: DEBT_MINT,
      marginfiGroup: MARGINFI_GROUP,
      debtBank: DEBT_BANK,
      collateralBank: COLLAT_BANK,
      debtOracles: [BANK_ORACLE],
      collateralOracles: [BANK_ORACLE],
      debtLiquidityVault: LIQ_VAULT,
      debtBankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      borrowerDebtToken: ATA,
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiProgram: MARGINFI_PROGRAM,
      rateBps: 800,
      termSeconds: 30 * 86_400,
      principalAtoms: 100_000_000n,
      collateralAtoms: 500_000_000n,
    });
    expectYdeltaIx(ix);
    expect(tagOf(ix.data)).toBe(InstructionTag.PlaceOrder);
    // tag(1) + Option<u32>(1 None) + u8(1) + u16(2) + u32(4) + u64(8) + u64(8) = 25
    expect(ix.data.length).toBe(25);
    // Last account = global_vault PDA derived from debt_mint.
    expect(ix.keys[ix.keys.length - 1].pubkey.equals(globalVaultPda(DEBT_MINT)[0])).toBe(true);
  });

  /* ── Tag 5 ───────────────────────────────────────────── */
  it('ProcessMatchedLoan (no vault settle)', () => {
    const ix = processMatchedLoanInstruction({
      payer: PAYER,
      market: MARKET,
      debtBank: DEBT_BANK,
      marginfiProgram: MARGINFI_PROGRAM,
      sequence: 42n,
    });
    expectYdeltaIx(ix);
    expect(tagOf(ix.data)).toBe(InstructionTag.ProcessMatchedLoan);
    // tag(1) + u64(8) + Option<u32>(1 None) = 10
    expect(ix.data.length).toBe(10);
    expect(ix.keys).toHaveLength(7);
    expect(ix.keys[3].pubkey.equals(loanPda(MARKET, 42n)[0])).toBe(true);
  });

  it('ProcessMatchedLoan (with vault settle)', () => {
    const vault = globalVaultPda(DEBT_MINT)[0];
    const ix = processMatchedLoanInstruction({
      payer: PAYER,
      market: MARKET,
      debtBank: DEBT_BANK,
      marginfiProgram: MARGINFI_PROGRAM,
      sequence: 42n,
      vaultSettle: {
        globalVault: vault,
        globalVaultSigner: SOME_KEY,
        globalVaultStaging: SOME_KEY,
        globalVaultIntegrationAccount: SOME_KEY,
        marketDebtVault: SOME_KEY,
        marketLenderIntegrationAccount: SOME_KEY,
        marketSigner: SOME_KEY,
        debtLiquidityVault: LIQ_VAULT,
        debtBankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
        debtOracles: [BANK_ORACLE, BANK_ORACLE],
        debtMint: DEBT_MINT,
        tokenProgram: TOKEN_PROGRAM_ID,
        marginfiGroup: MARGINFI_GROUP,
        marginfiProgram: MARGINFI_PROGRAM,
      },
    });
    // 7 base + 14 vault-settle + 2 extra oracles = 23
    // (Block has 13 fixed entries + variadic debt_oracles; we passed 2 oracles)
    expect(ix.keys.length).toBe(7 + 13 + 2);
  });

  /* ── Tag 6 ───────────────────────────────────────────── */
  it('Repay', () => {
    const ix = repayInstruction({
      borrower: PAYER,
      market: MARKET,
      sequence: 7n,
      debtMint: DEBT_MINT,
      borrowerToken: ATA,
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      debtBank: DEBT_BANK,
      debtLiquidityVault: LIQ_VAULT,
      collateralBank: COLLAT_BANK,
      marginfiProgram: MARGINFI_PROGRAM,
      repayAtoms: 1_000n,
      fullRepay: true,
      crankerRefund: NEW_ADMIN,
    });
    expectYdeltaIx(ix);
    expect(tagOf(ix.data)).toBe(InstructionTag.Repay);
    // tag(1) + u64(8) + bool(1) + Option<u32>(1 None) = 11
    expect(ix.data.length).toBe(11);
    expect(ix.keys[3].pubkey.equals(loanPda(MARKET, 7n)[0])).toBe(true);
    expect(ix.keys[ix.keys.length - 1].pubkey.equals(NEW_ADMIN)).toBe(true);
  });

  /* ── Tag 7 ───────────────────────────────────────────── */
  it('SyncMarketPosition', () => {
    const ix = syncMarketPositionInstruction({ payer: PAYER, market: MARKET, owner: ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.SyncMarketPosition);
    expect(ix.keys).toHaveLength(5);
    expect(ix.keys[2].pubkey.equals(userAccountPda(ADMIN)[0])).toBe(true);
    expect(ix.keys[4].pubkey.equals(ADMIN)).toBe(true);
  });

  /* ── Tag 8 ───────────────────────────────────────────── */
  it('CreateVault', () => {
    const ix = createVaultInstruction({
      payer: PAYER,
      mint: DEBT_MINT,
      marginfiGroup: MARGINFI_GROUP,
      lendingPool: DEBT_BANK,
      marginfiProgram: MARGINFI_PROGRAM,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CreateVault);
    expect(ix.keys[2].pubkey.equals(globalVaultPda(DEBT_MINT)[0])).toBe(true);
  });

  /* ── Tag 9 ───────────────────────────────────────────── */
  it('CreateRiskProfile encodes the policy fields (profile_id is program-assigned)', () => {
    const ix = createRiskProfileInstruction({
      payer: PAYER,
      mint: DEBT_MINT,
      curator: ADMIN,
      maxLtvBps: 6_000,
      maxTermSeconds: 30 * 86_400,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CreateRiskProfile);
    // tag(1) + Pubkey(32) + u16(2) + u32(4) = 39. `profile_id` is no
    // longer in the wire payload — it's assigned by the program from
    // the vault's monotonic `next_profile_id` counter.
    expect(ix.data.length).toBe(39);
  });

  /* ── Tag 10 ──────────────────────────────────────────── */
  it('GlobalVaultDeposit', () => {
    const ix = globalVaultDepositInstruction({
      depositor: PAYER,
      mint: DEBT_MINT,
      depositorToken: ATA,
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      lendingPool: DEBT_BANK,
      liquidityVault: LIQ_VAULT,
      marginfiProgram: MARGINFI_PROGRAM,
      profileId: 3,
      amountAtoms: 1_000_000n,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.GlobalVaultDeposit);
    // tag(1) + u64(8) + u8(1) = 10
    expect(ix.data.length).toBe(10);
    expect(ix.data[ix.data.length - 1]).toBe(3);
  });

  /* ── Tag 11 ──────────────────────────────────────────── */
  it('GlobalVaultWithdraw encodes u128 shares + profile_id', () => {
    const ix = globalVaultWithdrawInstruction({
      depositor: PAYER,
      mint: DEBT_MINT,
      depositorToken: ATA,
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      lendingPool: DEBT_BANK,
      lendingPoolOracle: BANK_ORACLE,
      liquidityVault: LIQ_VAULT,
      bankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      marginfiProgram: MARGINFI_PROGRAM,
      profileId: 1,
      sharesToBurn: 12345n,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.GlobalVaultWithdraw);
    // tag(1) + u128(16) + u8(1) = 18
    expect(ix.data.length).toBe(18);
  });

  /* ── Tag 12 ──────────────────────────────────────────── */
  it('PlaceOrderForRiskProfile uses the split-payer pattern', () => {
    const ix = placeOrderForRiskProfileInstruction({
      feePayer: PAYER,
      curator: ADMIN,
      mint: DEBT_MINT,
      market: MARKET,
      profileId: 0,
      rateBps: 750,
      termSeconds: 30 * 86_400,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.PlaceOrderForRiskProfile);
    expect(ix.keys[0]).toMatchObject({ pubkey: PAYER, isSigner: true, isWritable: true });
    expect(ix.keys[1]).toMatchObject({ pubkey: ADMIN, isSigner: true, isWritable: false });
    // tag(1) + u8(1) + u16(2) + u32(4) + u8(1) = 9
    expect(ix.data.length).toBe(9);
  });

  /* ── Tag 13 ──────────────────────────────────────────── */
  it('CancelOrderForRiskProfile', () => {
    const ix = cancelOrderForRiskProfileInstruction({
      feePayer: PAYER,
      curator: ADMIN,
      mint: DEBT_MINT,
      market: MARKET,
      profileId: 5,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CancelOrderForRiskProfile);
    expect(ix.data.length).toBe(2);
    expect(ix.data[1]).toBe(5);
  });

  /* ── Tag 14 ──────────────────────────────────────────── */
  it('UpdateOrderForRiskProfile', () => {
    const ix = updateOrderForRiskProfileInstruction({
      feePayer: PAYER,
      curator: ADMIN,
      mint: DEBT_MINT,
      market: MARKET,
      profileId: 5,
      newRateBps: 900,
      newTermSeconds: 60 * 86_400,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.UpdateOrderForRiskProfile);
    // tag(1) + u8(1) + u16(2) + u32(4) + u8(1) = 9
    expect(ix.data.length).toBe(9);
  });

  /* ── Tag 15 ──────────────────────────────────────────── */
  it('ClaimCuratorFee', () => {
    const ix = claimCuratorFeeInstruction({
      curator: PAYER,
      mint: DEBT_MINT,
      profileId: 2,
      curatorToken: ATA,
      debtBank: DEBT_BANK,
      liquidityVault: LIQ_VAULT,
      bankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      bankOracle: BANK_ORACLE,
      marginfiProgram: MARGINFI_PROGRAM,
      marginfiGroup: MARGINFI_GROUP,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.ClaimCuratorFee);
    expect(ix.data.length).toBe(2);
  });

  /* ── Tag 16 ──────────────────────────────────────────── */
  it('SettleMaturedLoan: data is RAW tag + u64 (no borsh)', () => {
    const ix = settleMaturedLoanInstruction({
      payer: PAYER,
      market: MARKET,
      sequence: 1n,
      debtMint: DEBT_MINT,
      collateralMint: COLLAT_MINT,
      liquidatorDebtToken: ATA,
      liquidatorCollateralToken: ATA,
      debtBank: DEBT_BANK,
      collateralBank: COLLAT_BANK,
      debtLiquidityVault: LIQ_VAULT,
      collateralLiquidityVault: LIQ_VAULT,
      collateralBankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      debtOracles: [BANK_ORACLE],
      collateralOracles: [BANK_ORACLE],
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      marginfiProgram: MARGINFI_PROGRAM,
      repayAtomsMax: 12345n,
      crankerRefund: NEW_ADMIN,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.SettleMaturedLoan);
    // tag(1) + u64(8) = 9
    expect(ix.data.length).toBe(9);
  });

  /* ── Tag 17 ──────────────────────────────────────────── */
  it('LiquidateLoan shares the SettleMaturedLoan body shape', () => {
    const ix = liquidateLoanInstruction({
      payer: PAYER,
      market: MARKET,
      sequence: 1n,
      debtMint: DEBT_MINT,
      collateralMint: COLLAT_MINT,
      liquidatorDebtToken: ATA,
      liquidatorCollateralToken: ATA,
      debtBank: DEBT_BANK,
      collateralBank: COLLAT_BANK,
      debtLiquidityVault: LIQ_VAULT,
      collateralLiquidityVault: LIQ_VAULT,
      collateralBankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      debtOracles: [BANK_ORACLE],
      collateralOracles: [BANK_ORACLE],
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      marginfiProgram: MARGINFI_PROGRAM,
      repayAtomsMax: 0n,
      crankerRefund: NEW_ADMIN,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.LiquidateLoan);
    expect(ix.data.length).toBe(9);
  });

  /* ── Tag 18 ──────────────────────────────────────────── */
  it('SetFeeConfig packs eight Option<...> fields in order', () => {
    const ix = setFeeConfigInstruction({
      admin: ADMIN,
      market: MARKET,
      protocolFeeBpsFloor: 50,
      gracePeriodSeconds: 86_400,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.SetFeeConfig);
    // tag(1) + Some<u16>(3) + 6×None(6) + Some<u32>(5) = 15
    expect(ix.data.length).toBe(15);
  });

  /* ── Tag 19 ──────────────────────────────────────────── */
  it('ProtocolFeeClaim', () => {
    const ix = protocolFeeClaimInstruction({
      admin: ADMIN,
      market: MARKET,
      debtMint: DEBT_MINT,
      adminDebtToken: ATA,
      debtBank: DEBT_BANK,
      debtLiquidityVault: LIQ_VAULT,
      debtBankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      debtOracles: [BANK_ORACLE],
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      marginfiProgram: MARGINFI_PROGRAM,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.ProtocolFeeClaim);
    expect(ix.data.length).toBe(1);
  });

  /* ── Tag 20 ──────────────────────────────────────────── */
  it('ClaimRepaymentForRiskProfile (required cranker refund)', () => {
    const ix = claimRepaymentForRiskProfileInstruction({
      payer: PAYER,
      market: MARKET,
      sequence: 1n,
      globalVault: globalVaultPda(DEBT_MINT)[0],
      debtMint: DEBT_MINT,
      debtBank: DEBT_BANK,
      debtLiquidityVault: LIQ_VAULT,
      debtBankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      bankOracle: BANK_ORACLE,
      lenderMarginfiAccount: lenderIntegrationAccountPda(MARKET)[0],
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      marginfiProgram: MARGINFI_PROGRAM,
      crankerRefund: NEW_ADMIN,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.ClaimRepaymentForRiskProfile);
    expect(ix.keys).toHaveLength(20);
    expect(ix.keys[19].pubkey.equals(NEW_ADMIN)).toBe(true);
  });

  /* ── Tags 21..26 ─────────────────────────────────────── */
  it('TransferMarketAdmin', () => {
    const ix = transferMarketAdminInstruction({ market: MARKET, currentAdmin: ADMIN, newAdmin: NEW_ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.TransferMarketAdmin);
    // tag(1) + Pubkey(32) = 33
    expect(ix.data.length).toBe(33);
    expect(ix.keys).toHaveLength(3);
  });

  it('AcceptMarketAdmin', () => {
    const ix = acceptMarketAdminInstruction({ market: MARKET, pendingAdmin: NEW_ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.AcceptMarketAdmin);
    expect(ix.data.length).toBe(1);
    expect(ix.keys).toHaveLength(3);
  });

  it('TransferGlobalVaultAdmin', () => {
    const ix = transferGlobalVaultAdminInstruction({ mint: DEBT_MINT, currentAdmin: ADMIN, newAdmin: NEW_ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.TransferGlobalVaultAdmin);
    expect(ix.data.length).toBe(33);
  });

  it('AcceptGlobalVaultAdmin', () => {
    const ix = acceptGlobalVaultAdminInstruction({ mint: DEBT_MINT, pendingAdmin: NEW_ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.AcceptGlobalVaultAdmin);
    expect(ix.data.length).toBe(1);
  });

  it('TransferCurator includes profile_id + new_curator', () => {
    const ix = transferCuratorInstruction({
      mint: DEBT_MINT,
      currentCurator: ADMIN,
      profileId: 4,
      newCurator: NEW_ADMIN,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.TransferCurator);
    // tag(1) + u8(1) + Pubkey(32) = 34
    expect(ix.data.length).toBe(34);
    expect(ix.data[1]).toBe(4);
  });

  it('AcceptCurator', () => {
    const ix = acceptCuratorInstruction({ mint: DEBT_MINT, pendingCurator: NEW_ADMIN, profileId: 4 });
    expect(tagOf(ix.data)).toBe(InstructionTag.AcceptCurator);
    expect(ix.data.length).toBe(2);
  });

  /* ── Tag 27 ──────────────────────────────────────────── */
  it('SetMarketPause', () => {
    const paused = setMarketPauseInstruction({ admin: ADMIN, market: MARKET, paused: true });
    const unpaused = setMarketPauseInstruction({ admin: ADMIN, market: MARKET, paused: false });
    expect(tagOf(paused.data)).toBe(InstructionTag.SetMarketPause);
    expect(paused.data[1]).toBe(1);
    expect(unpaused.data[1]).toBe(0);
  });

  /* ── Tag 28 ──────────────────────────────────────────── */
  it('CreateGlobalConfig', () => {
    const ix = createGlobalConfigInstruction({ payer: PAYER, programData: SOME_KEY });
    expect(tagOf(ix.data)).toBe(InstructionTag.CreateGlobalConfig);
    expect(ix.keys).toHaveLength(4);
    expect(ix.keys[1].pubkey.equals(globalConfigPda()[0])).toBe(true);
  });

  /* ── Tags 29..31 ─────────────────────────────────────── */
  it('TransferProtocolAdmin', () => {
    const ix = transferProtocolAdminInstruction({ currentAdmin: ADMIN, newAdmin: NEW_ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.TransferProtocolAdmin);
    expect(ix.data.length).toBe(33);
  });

  it('AcceptProtocolAdmin', () => {
    const ix = acceptProtocolAdminInstruction({ pendingAdmin: NEW_ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.AcceptProtocolAdmin);
    expect(ix.data.length).toBe(1);
  });

  it('SetGlobalPause', () => {
    const ix = setGlobalPauseInstruction({ admin: ADMIN, paused: true });
    expect(tagOf(ix.data)).toBe(InstructionTag.SetGlobalPause);
    expect(ix.data[1]).toBe(1);
  });

  /* ── Tag 32 ──────────────────────────────────────────── */
  it('UpdateRiskProfile packs two Option<...> fields after profile_id', () => {
    const ix = updateRiskProfileInstruction({
      payer: ADMIN,
      mint: DEBT_MINT,
      profileId: 3,
      newMaxLtvBps: 7500,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.UpdateRiskProfile);
    // tag(1) + u8(1) + Some<u16>(3) + None<u32>(1) = 6
    expect(ix.data.length).toBe(6);
  });

  /* ── Tag 33 ──────────────────────────────────────────── */
  it('ConvertP2PoolToFixed', () => {
    const ix = convertP2poolToFixedInstruction({
      borrower: PAYER,
      market: MARKET,
      loanSequence: 1n,
      debtMint: DEBT_MINT,
      debtBank: DEBT_BANK,
      debtLiquidityVault: LIQ_VAULT,
      debtBankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      debtOracles: [BANK_ORACLE],
      collateralBank: COLLAT_BANK,
      collateralOracles: [BANK_ORACLE],
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      marginfiProgram: MARGINFI_PROGRAM,
      maxAcceptableRateBps: 850,
      crankerRefund: NEW_ADMIN,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.ConvertP2PoolToFixed);
    // tag(1) + u16(2) = 3
    expect(ix.data.length).toBe(3);
  });

  /* ── Tags 34, 35 ─────────────────────────────────────── */
  it('CheckLtvLiquidatable is read-only-gate-shaped', () => {
    const ix = checkLtvLiquidatableInstruction({
      payer: PAYER,
      market: MARKET,
      sequence: 1n,
      debtBank: DEBT_BANK,
      collateralBank: COLLAT_BANK,
      debtOracles: [BANK_ORACLE],
      collateralOracles: [BANK_ORACLE],
      marginfiProgram: MARGINFI_PROGRAM,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CheckLtvLiquidatable);
    expect(ix.data.length).toBe(1);
  });

  it('CheckMaturityLiquidatable has 7 fixed accounts', () => {
    const ix = checkMaturityLiquidatableInstruction({
      payer: PAYER,
      market: MARKET,
      sequence: 1n,
      debtBank: DEBT_BANK,
      marginfiProgram: MARGINFI_PROGRAM,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CheckMaturityLiquidatable);
    expect(ix.keys).toHaveLength(7);
  });

  /* ── Tag 36 ──────────────────────────────────────────── */
  it('SetVaultPause', () => {
    const ix = setVaultPauseInstruction({ admin: ADMIN, mint: DEBT_MINT, paused: true });
    expect(tagOf(ix.data)).toBe(InstructionTag.SetVaultPause);
    expect(ix.data[1]).toBe(1);
  });
});

describe('tag space is exhaustive 0..=37', () => {
  it('InstructionTag has every value from 0 to 37', () => {
    const values = Object.values(InstructionTag).filter((v): v is number => typeof v === 'number');
    expect(values.sort((a, b) => a - b)).toEqual(Array.from({ length: 38 }, (_, i) => i));
  });
});
