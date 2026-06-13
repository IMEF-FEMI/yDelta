/**
 * Instruction-builder byte tests. For each of the 45 builders, verify:
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
 *
 * v1: the global vault is keyed by the marginfi BANK (a market's debt lending
 * pool), so vault-bearing builders derive `globalVaultPda(DEBT_BANK)`.
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
  adminCancelSubVaultOrderInstruction,
  borrowerIntegrationAccountPda,
  cancelOrderInstruction,
  cancelOrderForSubVaultInstruction,
  checkLtvLiquidatableInstruction,
  checkMaturityLiquidatableInstruction,
  claimCuratorFeeInstruction,
  claimRepaymentForSubVaultInstruction,
  claimSeatInstruction,
  convertP2poolToFixedInstruction,
  createGlobalConfigInstruction,
  createMarketInstruction,
  createPoolSubVaultInstruction,
  createPrivateSubVaultInstruction,
  createVaultInstruction,
  depositInstruction,
  globalConfigPda,
  globalVaultDepositInstruction,
  globalVaultPda,
  globalVaultWithdrawInstruction,
  lenderIntegrationAccountPda,
  liquidateLoanInstruction,
  loanPda,
  matchCrankInstruction,
  marketTokenVaultPda,
  placeOrderForSubVaultInstruction,
  placeOrderInstruction,
  processMatchedLoanInstruction,
  protocolFeeClaimInstruction,
  removeSubVaultInstruction,
  repayInstruction,
  resumeSubVaultInstruction,
  setFeeConfigInstruction,
  setGlobalPauseInstruction,
  setMarketPauseInstruction,
  setVaultPauseInstruction,
  settleMaturedLoanInstruction,
  sunsetSubVaultInstruction,
  syncMarketPositionInstruction,
  transferCuratorInstruction,
  transferGlobalVaultAdminInstruction,
  transferMarketAdminInstruction,
  transferProtocolAdminInstruction,
  updateOrderInstruction,
  updateOrderForSubVaultInstruction,
  updateSubVaultInstruction,
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
  it('CreateMarket (no params → 6 null Option flags)', () => {
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
    // 1-byte tag + 6 borsh Option flags (each 1 byte = 0 for None when omitted).
    // v1 dropped `curatorFeeBps` and `ltvBufferBps` from the params struct.
    expect(ix.data.length).toBe(7);
    expect(Array.from(ix.data.slice(1))).toEqual([0, 0, 0, 0, 0, 0]);
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
        protocolFeeBpsFloor: 50,
        gracePeriodSeconds: 3600,
      },
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CreateMarket);
    // tag(1) + Some<u16>(3) + 4×None(4) + Some<u32>(5) = 13 bytes.
    expect(ix.data.length).toBe(1 + 3 + 4 + 5);
    // protocolFeeBpsFloor = 50 (1st option, at offset 1).
    expect(ix.data[1]).toBe(1); // Some tag
    expect(ix.data.readUInt16LE(2)).toBe(50);
    // gracePeriodSeconds = 3600 (6th option, at offset 1 + 3 + 4 = 8).
    expect(ix.data[8]).toBe(1); // Some tag
    expect(ix.data.readUInt32LE(9)).toBe(3600);
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
  it('PlaceOrder (v1 residual_mode + last_valid_unix_ts replace flags)', () => {
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
      residualMode: 1,
      lastValidUnixTs: 1_700_000_000n,
    });
    expectYdeltaIx(ix);
    expect(tagOf(ix.data)).toBe(InstructionTag.PlaceOrder);
    // tag(1) + Option<u32>(1 None) + u8 residual(1) + i64(8) + u16(2) + u32(4)
    //  + u64(8) + u64(8) = 33
    expect(ix.data.length).toBe(33);
    expect(ix.data[2]).toBe(1); // residual_mode = Rest
    // Last account = global_vault PDA derived from the DEBT BANK (v1 keying).
    expect(ix.keys[ix.keys.length - 1].pubkey.equals(globalVaultPda(DEBT_BANK)[0])).toBe(true);
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
    const vault = globalVaultPda(DEBT_BANK)[0];
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
    // 7 base + 13 fixed vault-settle entries + 2 extra oracles = 22
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
    expect(ix.keys[2].pubkey.equals(globalVaultPda(DEBT_BANK)[0])).toBe(true);
  });

  /* ── Tag 9 ───────────────────────────────────────────── */
  it('CreatePoolSubVault encodes curator + policy fields (sub_vault_id program-assigned)', () => {
    const ix = createPoolSubVaultInstruction({
      payer: PAYER,
      bank: DEBT_BANK,
      curator: ADMIN,
      spreadBps: 150,
      maxLtvBps: 6_000,
      liquidationLtvBps: 8_000,
      maxTermSeconds: 30 * 86_400,
      curatorFeeBps: 50,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CreatePoolSubVault);
    // tag(1) + Pubkey(32) + u16(2) + u16(2) + u16(2) + u32(4) + u16(2) = 45.
    // `sub_vault_id` is assigned by the program from `next_sub_vault_id`.
    expect(ix.data.length).toBe(45);
    // Protocol-admin-gated: payer(signer)/global_config/vault/system_program.
    expect(ix.keys).toHaveLength(4);
    expect(ix.keys[0]).toMatchObject({ pubkey: PAYER, isSigner: true });
    expect(ix.keys[2].pubkey.equals(globalVaultPda(DEBT_BANK)[0])).toBe(true);
  });

  /* ── Tag 10 ──────────────────────────────────────────── */
  it('GlobalVaultDeposit encodes u16 sub_vault_id', () => {
    const ix = globalVaultDepositInstruction({
      depositor: PAYER,
      mint: DEBT_MINT,
      depositorToken: ATA,
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      lendingPool: DEBT_BANK,
      liquidityVault: LIQ_VAULT,
      marginfiProgram: MARGINFI_PROGRAM,
      subVaultId: 3,
      amountAtoms: 1_000_000n,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.GlobalVaultDeposit);
    // tag(1) + u64(8) + u16(2) = 11
    expect(ix.data.length).toBe(11);
    expect(ix.data.readUInt16LE(9)).toBe(3);
  });

  /* ── Tag 11 ──────────────────────────────────────────── */
  it('GlobalVaultWithdraw encodes u128 shares + u16 sub_vault_id', () => {
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
      subVaultId: 1,
      sharesToBurn: 12345n,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.GlobalVaultWithdraw);
    // tag(1) + u128(16) + u16(2) = 19
    expect(ix.data.length).toBe(19);
    expect(ix.data.readUInt16LE(17)).toBe(1);
  });

  /* ── Tag 12 ──────────────────────────────────────────── */
  it('PlaceOrderForSubVault uses the split-payer pattern (no rate/term args)', () => {
    const ix = placeOrderForSubVaultInstruction({
      feePayer: PAYER,
      curator: ADMIN,
      market: MARKET,
      debtBank: DEBT_BANK,
      marginfiGroup: MARGINFI_GROUP,
      collateralBank: COLLAT_BANK,
      debtOracles: [BANK_ORACLE],
      collateralOracles: [BANK_ORACLE],
      subVaultId: 1,
      flags: 0,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.PlaceOrderForSubVault);
    expect(ix.keys[0]).toMatchObject({ pubkey: PAYER, isSigner: true, isWritable: true });
    expect(ix.keys[1]).toMatchObject({ pubkey: ADMIN, isSigner: true, isWritable: false });
    // tag(1) + u16 sub_vault_id(2) + u8 flags(1) = 4
    expect(ix.data.length).toBe(4);
    expect(ix.data.readUInt16LE(1)).toBe(1);
    // vault derived from DEBT BANK.
    expect(ix.keys[3].pubkey.equals(globalVaultPda(DEBT_BANK)[0])).toBe(true);
  });

  /* ── Tag 13 ──────────────────────────────────────────── */
  it('CancelOrderForSubVault encodes u16 sub_vault_id', () => {
    const ix = cancelOrderForSubVaultInstruction({
      feePayer: PAYER,
      curator: ADMIN,
      debtBank: DEBT_BANK,
      market: MARKET,
      subVaultId: 5,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CancelOrderForSubVault);
    // tag(1) + u16(2) = 3
    expect(ix.data.length).toBe(3);
    expect(ix.data.readUInt16LE(1)).toBe(5);
  });

  /* ── Tag 14 ──────────────────────────────────────────── */
  it('UpdateOrderForSubVault encodes u16 sub_vault_id + u8 new_flags', () => {
    const ix = updateOrderForSubVaultInstruction({
      feePayer: PAYER,
      curator: ADMIN,
      market: MARKET,
      debtBank: DEBT_BANK,
      marginfiGroup: MARGINFI_GROUP,
      collateralBank: COLLAT_BANK,
      debtOracles: [BANK_ORACLE],
      collateralOracles: [BANK_ORACLE],
      subVaultId: 5,
      newFlags: 0,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.UpdateOrderForSubVault);
    // tag(1) + u16(2) + u8(1) = 4 (NO rate/term in v1)
    expect(ix.data.length).toBe(4);
    expect(ix.data.readUInt16LE(1)).toBe(5);
  });

  /* ── Tag 15 ──────────────────────────────────────────── */
  it('ClaimCuratorFee', () => {
    const ix = claimCuratorFeeInstruction({
      curator: PAYER,
      mint: DEBT_MINT,
      subVaultId: 2,
      curatorToken: ATA,
      debtBank: DEBT_BANK,
      liquidityVault: LIQ_VAULT,
      bankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      bankOracle: BANK_ORACLE,
      marginfiProgram: MARGINFI_PROGRAM,
      marginfiGroup: MARGINFI_GROUP,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.ClaimCuratorFee);
    // v1: data is tag(1) + sub_vault_id u16(2) = 3 bytes.
    expect(ix.data.length).toBe(3);
    expect(ix.data.readUInt16LE(1)).toBe(2);
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
  it('SetFeeConfig packs six Option<...> fields in order (no curator_fee/ltv_buffer)', () => {
    const ix = setFeeConfigInstruction({
      admin: ADMIN,
      market: MARKET,
      protocolFeeBpsFloor: 50,
      gracePeriodSeconds: 86_400,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.SetFeeConfig);
    // tag(1) + Some<u16>(3) + 4×None(4) + Some<u32>(5) = 13
    expect(ix.data.length).toBe(13);
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
  it('ClaimRepaymentForSubVault encodes u16 sub_vault_id', () => {
    const ix = claimRepaymentForSubVaultInstruction({
      payer: PAYER,
      market: MARKET,
      subVaultId: 4,
      globalVault: globalVaultPda(DEBT_BANK)[0],
      debtMint: DEBT_MINT,
      debtBank: DEBT_BANK,
      debtLiquidityVault: LIQ_VAULT,
      debtBankLiquidityVaultAuthority: LIQ_VAULT_AUTH,
      bankOracles: [BANK_ORACLE],
      lenderMarginfiAccount: lenderIntegrationAccountPda(MARKET)[0],
      tokenProgram: TOKEN_PROGRAM_ID,
      marginfiGroup: MARGINFI_GROUP,
      marginfiProgram: MARGINFI_PROGRAM,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.ClaimRepaymentForSubVault);
    // tag(1) + u16(2) = 3 (was empty in v0)
    expect(ix.data.length).toBe(3);
    expect(ix.data.readUInt16LE(1)).toBe(4);
    // 13 fixed accounts + 1 oracle + 4 trailing = 18. No cranker_refund.
    expect(ix.keys).toHaveLength(18);
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
    const ix = transferGlobalVaultAdminInstruction({ bank: DEBT_BANK, currentAdmin: ADMIN, newAdmin: NEW_ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.TransferGlobalVaultAdmin);
    expect(ix.data.length).toBe(33);
  });

  it('AcceptGlobalVaultAdmin', () => {
    const ix = acceptGlobalVaultAdminInstruction({ bank: DEBT_BANK, pendingAdmin: NEW_ADMIN });
    expect(tagOf(ix.data)).toBe(InstructionTag.AcceptGlobalVaultAdmin);
    expect(ix.data.length).toBe(1);
  });

  it('TransferCurator includes u16 sub_vault_id + new_curator', () => {
    const ix = transferCuratorInstruction({
      bank: DEBT_BANK,
      currentCurator: ADMIN,
      subVaultId: 4,
      newCurator: NEW_ADMIN,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.TransferCurator);
    // tag(1) + u16(2) + Pubkey(32) = 35
    expect(ix.data.length).toBe(35);
    expect(ix.data.readUInt16LE(1)).toBe(4);
  });

  it('AcceptCurator encodes u16 sub_vault_id', () => {
    const ix = acceptCuratorInstruction({ bank: DEBT_BANK, pendingCurator: NEW_ADMIN, subVaultId: 4 });
    expect(tagOf(ix.data)).toBe(InstructionTag.AcceptCurator);
    // tag(1) + u16(2) = 3
    expect(ix.data.length).toBe(3);
    expect(ix.data.readUInt16LE(1)).toBe(4);
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
  it('UpdateSubVault packs u16 id + four Option<...> fields', () => {
    const ix = updateSubVaultInstruction({
      payer: ADMIN,
      bank: DEBT_BANK,
      subVaultId: 3,
      newMaxLtvBps: 7500,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.UpdateSubVault);
    // tag(1) + u16(2) + Some<u16>(3) + None<u16>(1) + None<u32>(1) + None<u16>(1) = 9
    expect(ix.data.length).toBe(9);
    expect(ix.data.readUInt16LE(1)).toBe(3);
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
    const ix = setVaultPauseInstruction({ admin: ADMIN, bank: DEBT_BANK, paused: true });
    expect(tagOf(ix.data)).toBe(InstructionTag.SetVaultPause);
    expect(ix.data[1]).toBe(1);
  });

  /* ── Tag 37 ──────────────────────────────────────────── */
  it('RemoveSubVault encodes u16 sub_vault_id', () => {
    const ix = removeSubVaultInstruction({ payer: ADMIN, bank: DEBT_BANK, subVaultId: 9 });
    expect(tagOf(ix.data)).toBe(InstructionTag.RemoveSubVault);
    // tag(1) + u16(2) = 3
    expect(ix.data.length).toBe(3);
    expect(ix.data.readUInt16LE(1)).toBe(9);
    expect(ix.keys).toHaveLength(3);
  });

  /* ── Tag 38 ──────────────────────────────────────────── */
  it('SunsetSubVault encodes u16 sub_vault_id', () => {
    const ix = sunsetSubVaultInstruction({ payer: ADMIN, bank: DEBT_BANK, subVaultId: 2 });
    expect(tagOf(ix.data)).toBe(InstructionTag.SunsetSubVault);
    expect(ix.data.length).toBe(3);
    expect(ix.data.readUInt16LE(1)).toBe(2);
  });

  /* ── Tag 39 ──────────────────────────────────────────── */
  it('ResumeSubVault encodes u16 sub_vault_id', () => {
    const ix = resumeSubVaultInstruction({ payer: ADMIN, bank: DEBT_BANK, subVaultId: 2 });
    expect(tagOf(ix.data)).toBe(InstructionTag.ResumeSubVault);
    expect(ix.data.length).toBe(3);
    expect(ix.data.readUInt16LE(1)).toBe(2);
  });

  /* ── Tag 40 ──────────────────────────────────────────── */
  it('AdminCancelSubVaultOrder encodes u16 sub_vault_id', () => {
    const ix = adminCancelSubVaultOrderInstruction({
      payer: ADMIN,
      debtBank: DEBT_BANK,
      market: MARKET,
      subVaultId: 2,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.AdminCancelSubVaultOrder);
    expect(ix.data.length).toBe(3);
    expect(ix.data.readUInt16LE(1)).toBe(2);
    expect(ix.keys[3].pubkey.equals(MARKET)).toBe(true);
  });

  /* ── Tag 41 ──────────────────────────────────────────── */
  it('CreatePrivateSubVault encodes the policy fields (no curator/fee)', () => {
    const ix = createPrivateSubVaultInstruction({
      payer: PAYER,
      bank: DEBT_BANK,
      spreadBps: 150,
      maxLtvBps: 6_000,
      liquidationLtvBps: 8_000,
      maxTermSeconds: 30 * 86_400,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.CreatePrivateSubVault);
    // tag(1) + u16(2) + u16(2) + u16(2) + u32(4) = 11
    expect(ix.data.length).toBe(11);
    expect(ix.keys).toHaveLength(4);
    expect(ix.keys[0]).toMatchObject({ pubkey: PAYER, isSigner: true });
    expect(ix.keys[2].pubkey.equals(globalVaultPda(DEBT_BANK)[0])).toBe(true);
  });

  /* ── Tag 42 ──────────────────────────────────────────── */
  it('CancelOrder (user-signed) encodes u64 sequence + Option<u32> hint', () => {
    const ix = cancelOrderInstruction({ payer: PAYER, market: MARKET, orderSequence: 7n });
    expect(tagOf(ix.data)).toBe(InstructionTag.CancelOrder);
    // tag(1) + u64(8) + Option<u32>(1 None) = 10
    expect(ix.data.length).toBe(10);
    expect(ix.keys).toHaveLength(4);
    expect(ix.keys[0]).toMatchObject({ pubkey: PAYER, isSigner: true, isWritable: true });
    expect(ix.keys[1].pubkey.equals(globalConfigPda()[0])).toBe(true);
    expect(ix.keys[3].pubkey.equals(userAccountPda(PAYER)[0])).toBe(true);
  });

  /* ── Tag 43 ──────────────────────────────────────────── */
  it('MatchCrank encodes u32 max_fills, vault keyed by debt bank', () => {
    const ix = matchCrankInstruction({
      payer: PAYER,
      market: MARKET,
      debtBank: DEBT_BANK,
      marginfiGroup: MARGINFI_GROUP,
      collateralBank: COLLAT_BANK,
      debtOracles: [BANK_ORACLE],
      collateralOracles: [BANK_ORACLE],
      maxFills: 5,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.MatchCrank);
    // tag(1) + u32(4) = 5
    expect(ix.data.length).toBe(5);
    expect(ix.data.readUInt32LE(1)).toBe(5);
    expect(ix.keys[2].pubkey.equals(globalVaultPda(DEBT_BANK)[0])).toBe(true);
    // 7 fixed + 1 debt oracle + 1 collateral oracle + system_program = 10
    expect(ix.keys).toHaveLength(10);
  });

  /* ── Tag 44 ──────────────────────────────────────────── */
  it('UpdateOrder (user-signed) encodes seq + hint + rate/term/expiry', () => {
    const ix = updateOrderInstruction({
      payer: PAYER,
      market: MARKET,
      orderSequence: 7n,
      newRateBps: 900,
      newTermSeconds: 60 * 86_400,
      newLastValidUnixTs: 1_700_000_000n,
    });
    expect(tagOf(ix.data)).toBe(InstructionTag.UpdateOrder);
    // tag(1) + u64(8) + Option<u32>(1 None) + u16(2) + u32(4) + i64(8) = 24
    expect(ix.data.length).toBe(24);
    expect(ix.keys).toHaveLength(4);
    expect(ix.keys[3].pubkey.equals(userAccountPda(PAYER)[0])).toBe(true);
  });
});

describe('tag space is exhaustive 0..=44', () => {
  it('InstructionTag has every value from 0 to 44', () => {
    const values = Object.values(InstructionTag).filter((v): v is number => typeof v === 'number');
    expect(values.sort((a, b) => a - b)).toEqual(Array.from({ length: 45 }, (_, i) => i));
  });
});
