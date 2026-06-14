/**
 * Decoder round-trip tests. For each `decodeX`, construct a synthetic byte
 * buffer with known field values at the documented offsets, then assert the
 * decoder returns the same values.
 *
 * The byte layouts in `ts/src/accounts/*.ts` are derived from byte-offset
 * comments in `programs/ydelta/src/state/*.rs`. These tests catch decoder
 * regressions if those offsets ever drift.
 */
import { describe, expect, it } from 'vitest';
import { PublicKey } from '@solana/web3.js';

import {
  CLAIMED_SEAT_SIZE,
  GLOBAL_CONFIG_SIZE,
  LOAN_FIXED_SIZE,
  LOAN_FIXED_DISCRIMINANT,
  MARKET_FIXED_SIZE,
  GLOBAL_VAULT_FIXED_SIZE,
  SUB_VAULT_SIZE,
  decodeClaimedSeat,
  decodeGlobalConfig,
  decodeLoanFixed,
  decodeMarketHeader,
  decodeMatchedLoan,
  decodeRestingOrder,
  decodeSubVault,
  decodeGlobalVaultHeader,
  walkDescending,
  walkAscending,
  walkFreeList,
} from '../src/index.js';
import { NIL_INDEX } from '../src/accounts/_read.js';

function writeU64LE(buf: Uint8Array, off: number, val: bigint): void {
  new DataView(buf.buffer, buf.byteOffset, buf.byteLength).setBigUint64(off, val, true);
}
function writeI64LE(buf: Uint8Array, off: number, val: bigint): void {
  new DataView(buf.buffer, buf.byteOffset, buf.byteLength).setBigInt64(off, val, true);
}
function writeU32LE(buf: Uint8Array, off: number, val: number): void {
  new DataView(buf.buffer, buf.byteOffset, buf.byteLength).setUint32(off, val, true);
}
function writeU16LE(buf: Uint8Array, off: number, val: number): void {
  new DataView(buf.buffer, buf.byteOffset, buf.byteLength).setUint16(off, val, true);
}
function writeU128LE(buf: Uint8Array, off: number, val: bigint): void {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  dv.setBigUint64(off, val & 0xffff_ffff_ffff_ffffn, true);
  dv.setBigUint64(off + 8, val >> 64n, true);
}
function writePk(buf: Uint8Array, off: number, key: PublicKey): void {
  buf.set(key.toBuffer(), off);
}

const DISC_GLOBAL_CONFIG = 0x79_64_65_6c_74_61_47_63n;
const DISC_MARKET_FIXED = 0x79_64_65_6c_74_61_4d_4bn;
const DISC_GLOBAL_VAULT = 0x79_64_65_6c_74_61_56_61n;

describe('decodeGlobalConfig', () => {
  it('round-trips protocol_admin + pending_admin + is_paused', () => {
    const buf = new Uint8Array(GLOBAL_CONFIG_SIZE);
    writeU64LE(buf, 0, DISC_GLOBAL_CONFIG);
    const admin = new PublicKey('11111111111111111111111111111112');
    const pending = new PublicKey('11111111111111111111111111111113');
    writePk(buf, 8, admin);
    writePk(buf, 40, pending);
    buf[72] = 1;
    const decoded = decodeGlobalConfig(buf);
    expect(decoded.protocolAdmin.equals(admin)).toBe(true);
    expect(decoded.pendingProtocolAdmin.equals(pending)).toBe(true);
    expect(decoded.isPaused).toBe(true);
  });

  it('rejects bad discriminator', () => {
    const buf = new Uint8Array(GLOBAL_CONFIG_SIZE);
    writeU64LE(buf, 0, 0xdeadbeefn);
    expect(() => decodeGlobalConfig(buf)).toThrow();
  });

  it('rejects short buffer', () => {
    expect(() => decodeGlobalConfig(new Uint8Array(64))).toThrow();
  });
});

describe('decodeClaimedSeat', () => {
  it('reads u128 shares + owner_kind + sub_vault_id (u16 @106)', () => {
    const buf = new Uint8Array(CLAIMED_SEAT_SIZE);
    const owner = new PublicKey('11111111111111111111111111111112');
    writePk(buf, 0, owner);
    writeU128LE(buf, 32, 1_000_000n); // debt_withdrawable
    writeU128LE(buf, 48, 2_000_000n); // debt_encumbered
    writeU128LE(buf, 64, 3_000_000n); // collateral_withdrawable
    writeU128LE(buf, 80, 4_000_000n); // collateral_encumbered
    writeU32LE(buf, 96, 5);            // open_borrow_count
    writeU32LE(buf, 100, 7);           // open_lend_count
    buf[104] = 1;                      // owner_kind = SubVault
    writeU16LE(buf, 106, 9);           // sub_vault_id (u16)
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const seat = decodeClaimedSeat(dv);
    expect(seat.owner.equals(owner)).toBe(true);
    expect(seat.debtWithdrawableShares).toBe(1_000_000n);
    expect(seat.debtEncumberedShares).toBe(2_000_000n);
    expect(seat.collateralWithdrawableShares).toBe(3_000_000n);
    expect(seat.collateralEncumberedShares).toBe(4_000_000n);
    expect(seat.openBorrowCount).toBe(5);
    expect(seat.openLendCount).toBe(7);
    expect(seat.ownerKind).toBe(1);
    expect(seat.subVaultId).toBe(9);
  });

  it('reads a wide sub_vault_id (u16, not u8)', () => {
    const buf = new Uint8Array(CLAIMED_SEAT_SIZE);
    writeU16LE(buf, 106, 0x0102); // 258 — needs both bytes
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    expect(decodeClaimedSeat(dv).subVaultId).toBe(0x0102);
  });
});

describe('decodeRestingOrder', () => {
  it('reads rate / term / sequence + Side/OrderType bytes', () => {
    const buf = new Uint8Array(144);
    writeU32LE(buf, 0, 0x1234_5678); // trader_seat_index
    writeU64LE(buf, 8, 999n);          // sequence_number
    writeU64LE(buf, 16, 50_000n);      // principal_atoms
    writeU64LE(buf, 24, 250_000n);     // collateral_atoms
    writeI64LE(buf, 32, 0n);           // last_valid_unix_ts (no expiry)
    writeU32LE(buf, 40, 86_400 * 30);  // term_seconds
    writeU16LE(buf, 44, 750);          // rate_bps
    buf[46] = 1;                       // side = Ask
    buf[47] = 0;                       // order_type = Limit
    buf[48] = 0;                       // flags
    writeU128LE(buf, 50, 1n << 48n);   // share_price_snapshot_fp48 = 1.0
    writeU16LE(buf, 66, 1_250);        // ltv_buffer_bps (pins offset 66)
    const dv = new DataView(buf.buffer);
    const order = decodeRestingOrder(dv);
    expect(order.traderSeatIndex).toBe(0x12345678);
    expect(order.sequenceNumber).toBe(999n);
    expect(order.principalAtoms).toBe(50_000n);
    expect(order.collateralAtoms).toBe(250_000n);
    expect(order.lastValidUnixTs).toBe(0n);
    expect(order.termSeconds).toBe(86_400 * 30);
    expect(order.rateBps).toBe(750);
    expect(order.side).toBe(1);
    expect(order.sharePriceSnapshotFp48).toBe(1n << 48n);
    expect(order.ltvBufferBps).toBe(1_250);
  });

  it('reads a non-zero last_valid_unix_ts expiry', () => {
    const buf = new Uint8Array(144);
    writeI64LE(buf, 32, 1_700_000_000n);
    const dv = new DataView(buf.buffer);
    expect(decodeRestingOrder(dv).lastValidUnixTs).toBe(1_700_000_000n);
  });
});

describe('decodeMatchedLoan', () => {
  it('reads sequence + principal + rates + the v1 ltv/curator snapshots', () => {
    const buf = new Uint8Array(144);
    writeU64LE(buf, 0, 42n);                  // sequence
    writeU128LE(buf, 16, 0n);                 // borrower_marginfi_borrow_shares
    writeU64LE(buf, 32, 100_000n);            // principal_atoms
    writeU64LE(buf, 40, 100n);                // origination_atoms
    writeU64LE(buf, 48, 500_000n);            // collateral_atoms
    writeI64LE(buf, 56, 1_700_000_000n);      // matched_at_unix
    writeU32LE(buf, 64, 0x100);                // lender_seat_index
    writeU32LE(buf, 68, 0x200);                // borrower_seat_index
    writeU32LE(buf, 72, 86_400 * 30);          // term_seconds
    writeU16LE(buf, 76, 830);                  // borrower_rate_bps
    writeU16LE(buf, 78, 800);                  // lender_rate_bps
    buf[80] = 0;                               // loan_type = Fixed
    buf[81] = 0b0000_0001;                     // flags = VAULT_LENDER
    writeU16LE(buf, 88, 25);                   // curator_fee_bps_snapshot
    writeU16LE(buf, 90, 6_000);                // origination_ltv_bps
    writeU16LE(buf, 92, 8_000);                // liquidation_ltv_bps
    writeU128LE(buf, 112, 1n << 48n);          // lender_debt_share_price_snapshot_fp48
    writeU128LE(buf, 128, 2n << 48n);          // borrower_collateral_share_price_snapshot_fp48
    const dv = new DataView(buf.buffer);
    const loan = decodeMatchedLoan(dv);
    expect(loan.sequence).toBe(42n);
    expect(loan.principalAtoms).toBe(100_000n);
    expect(loan.borrowerRateBps).toBe(830);
    expect(loan.lenderRateBps).toBe(800);
    expect(loan.flags & 0b1).toBe(1); // VAULT_LENDER bit
    expect(loan.curatorFeeBpsSnapshot).toBe(25);
    expect(loan.originationLtvBps).toBe(6_000);
    expect(loan.liquidationLtvBps).toBe(8_000);
    expect(loan.lenderDebtSharePriceSnapshotFp48).toBe(1n << 48n);
    expect(loan.borrowerCollateralSharePriceSnapshotFp48).toBe(2n << 48n);
  });
});

describe('decodeSubVault', () => {
  it('reads the full 496-byte sub-vault layout', () => {
    const buf = new Uint8Array(SUB_VAULT_SIZE);
    writeU16LE(buf, 0, 7);                     // sub_vault_id (u16)
    buf[2] = 1;                                 // is_sunset
    buf[3] = 1;                                 // kind = Private
    writeU16LE(buf, 4, 150);                   // spread_bps
    const curator = new PublicKey('11111111111111111111111111111112');
    writePk(buf, 8, curator);
    writeU16LE(buf, 40, 6_000);                // max_ltv_bps
    writeU16LE(buf, 42, 8_000);                // liquidation_ltv_bps
    writeU32LE(buf, 44, 86_400 * 90);          // max_term_seconds
    writeU16LE(buf, 48, 50);                   // curator_fee_bps
    writeU16LE(buf, 50, 3);                    // open_orders_count (u16)
    writeU32LE(buf, 52, 11);                   // open_loans_count
    writeU128LE(buf, 64, 1_000_000n);          // total_shares
    writeU64LE(buf, 80, 2_000_000n);           // total_assets_atoms
    writeU64LE(buf, 88, 1_500_000n);           // total_principal_atoms
    writeU64LE(buf, 96, 900_000n);             // deployed_principal_atoms
    writeU64LE(buf, 104, 100_000n);            // encumbered_in_orders_atoms
    writeU128LE(buf, 112, 5_000n);             // total_weighted_rate_bps
    writeU64LE(buf, 128, 4_242n);              // accumulated_curator_fee_atoms
    writeI64LE(buf, 136, 1_700_000_000n);      // last_accrue_unix
    writeU128LE(buf, 144, 1n << 64n);          // cumulative_supply_yield_index_scaled
    writeU128LE(buf, 160, 2n << 64n);          // cumulative_delta_yield_index_scaled
    writeU128LE(buf, 176, 1n << 48n);          // last_supply_share_value_fp48
    const pending = new PublicKey('11111111111111111111111111111113');
    writePk(buf, 192, pending);
    writeU128LE(buf, 224, 4_500n);             // total_weighted_net_rate_bps
    writeU64LE(buf, 240, 777n);                // pending_claim_atoms
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const sv = decodeSubVault(dv);
    expect(sv.subVaultId).toBe(7);
    expect(sv.isSunset).toBe(1);
    expect(sv.kind).toBe(1);
    expect(sv.spreadBps).toBe(150);
    expect(sv.curator.equals(curator)).toBe(true);
    expect(sv.maxLtvBps).toBe(6_000);
    expect(sv.liquidationLtvBps).toBe(8_000);
    expect(sv.maxTermSeconds).toBe(86_400 * 90);
    expect(sv.curatorFeeBps).toBe(50);
    expect(sv.openOrdersCount).toBe(3);
    expect(sv.openLoansCount).toBe(11);
    expect(sv.totalShares).toBe(1_000_000n);
    expect(sv.totalAssetsAtoms).toBe(2_000_000n);
    expect(sv.totalPrincipalAtoms).toBe(1_500_000n);
    expect(sv.deployedPrincipalAtoms).toBe(900_000n);
    expect(sv.encumberedInOrdersAtoms).toBe(100_000n);
    expect(sv.totalWeightedRateBps).toBe(5_000n);
    expect(sv.accumulatedCuratorFeeAtoms).toBe(4_242n);
    expect(sv.lastAccrueUnix).toBe(1_700_000_000n);
    expect(sv.cumulativeSupplyYieldIndexScaled).toBe(1n << 64n);
    expect(sv.cumulativeDeltaYieldIndexScaled).toBe(2n << 64n);
    expect(sv.lastSupplyShareValueFp48).toBe(1n << 48n);
    expect(sv.pendingCurator.equals(pending)).toBe(true);
    expect(sv.totalWeightedNetRateBps).toBe(4_500n);
    expect(sv.pendingClaimAtoms).toBe(777n);
  });
});

describe('decodeLoanFixed', () => {
  it('reads the 288-byte loan layout incl. v1 ltv + u16 lender_sub_vault_id', () => {
    const buf = new Uint8Array(LOAN_FIXED_SIZE);
    writeU64LE(buf, 0, LOAN_FIXED_DISCRIMINANT);
    const market = new PublicKey('CYf9nJB7eJYqVRm6ucMcRYrwvtT6mzS5HHKkc4dHHGxk');
    const createdBy = new PublicKey('11111111111111111111111111111112');
    writePk(buf, 8, market);
    writePk(buf, 40, createdBy);
    writeU64LE(buf, 72, 99n);                  // matched_loan_sequence
    writeU128LE(buf, 80, 0n);                  // borrower_marginfi_borrow_shares
    writeU16LE(buf, 96, 6_000);                // origination_ltv_bps
    writeU16LE(buf, 98, 8_000);                // liquidation_ltv_bps
    writeU64LE(buf, 112, 1_000_000n);          // principal_debt_atoms
    writeU64LE(buf, 120, 1_010_000n);          // outstanding_debt_atoms
    writeU64LE(buf, 128, 1_005_000n);          // lender_claimable_atoms
    writeU64LE(buf, 136, 5_000_000n);          // collateral_atoms
    writeU64LE(buf, 144, 50n);                 // accumulated_protocol_fee_atoms
    writeU64LE(buf, 152, 30n);                 // accumulated_curator_fee_atoms
    writeI64LE(buf, 160, 1_700_000_000n);      // started_at_unix
    writeI64LE(buf, 168, 1_702_000_000n);      // matures_at_unix
    writeI64LE(buf, 176, 1_700_500_000n);      // last_accrued_unix
    writeU32LE(buf, 184, 4);                   // lender_seat_index
    writeU32LE(buf, 188, 9);                   // borrower_seat_index
    writeU16LE(buf, 192, 830);                 // borrower_rate_bps
    writeU16LE(buf, 194, 800);                 // lender_rate_bps
    buf[196] = 0;                              // state = Active
    buf[197] = 0;                              // loan_type = Fixed
    buf[198] = 0b0000_0001;                    // flags
    buf[199] = 1;                              // version
    buf[200] = 254;                            // bump
    buf[201] = 1;                              // lender_kind = GlobalVault
    writeU16LE(buf, 202, 0x0103);              // lender_sub_vault_id (u16 — 259)
    writeU16LE(buf, 204, 25);                  // curator_fee_bps_snapshot
    const lenderVault = new PublicKey('11111111111111111111111111111113');
    writePk(buf, 208, lenderVault);
    writeU64LE(buf, 240, 12_000n);             // principal_retired_atoms
    writeU64LE(buf, 248, 6_000n);              // cumulative_lender_gross_interest_atoms
    writeU128LE(buf, 256, 1n << 48n);          // lender_debt_share_price_snapshot_fp48
    writeU128LE(buf, 272, 2n << 48n);          // borrower_collateral_share_price_snapshot_fp48
    const loan = decodeLoanFixed(buf);
    expect(loan.market.equals(market)).toBe(true);
    expect(loan.createdBy.equals(createdBy)).toBe(true);
    expect(loan.matchedLoanSequence).toBe(99n);
    expect(loan.originationLtvBps).toBe(6_000);
    expect(loan.liquidationLtvBps).toBe(8_000);
    expect(loan.principalDebtAtoms).toBe(1_000_000n);
    expect(loan.outstandingDebtAtoms).toBe(1_010_000n);
    expect(loan.lenderClaimableAtoms).toBe(1_005_000n);
    expect(loan.collateralAtoms).toBe(5_000_000n);
    expect(loan.accumulatedProtocolFeeAtoms).toBe(50n);
    expect(loan.accumulatedCuratorFeeAtoms).toBe(30n);
    expect(loan.startedAtUnix).toBe(1_700_000_000n);
    expect(loan.maturesAtUnix).toBe(1_702_000_000n);
    expect(loan.lastAccruedUnix).toBe(1_700_500_000n);
    expect(loan.lenderSeatIndex).toBe(4);
    expect(loan.borrowerSeatIndex).toBe(9);
    expect(loan.borrowerRateBps).toBe(830);
    expect(loan.lenderRateBps).toBe(800);
    expect(loan.state).toBe(0);
    expect(loan.version).toBe(1);
    expect(loan.bump).toBe(254);
    expect(loan.lenderKind).toBe(1);
    expect(loan.lenderSubVaultId).toBe(0x0103);
    expect(loan.curatorFeeBpsSnapshot).toBe(25);
    expect(loan.lenderGlobalVault.equals(lenderVault)).toBe(true);
    expect(loan.principalRetiredAtoms).toBe(12_000n);
    expect(loan.cumulativeLenderGrossInterestAtoms).toBe(6_000n);
    expect(loan.lenderDebtSharePriceSnapshotFp48).toBe(1n << 48n);
    expect(loan.borrowerCollateralSharePriceSnapshotFp48).toBe(2n << 48n);
  });

  it('rejects a bad discriminator', () => {
    const buf = new Uint8Array(LOAN_FIXED_SIZE);
    writeU64LE(buf, 0, 0xdeadbeefn);
    expect(() => decodeLoanFixed(buf)).toThrow();
  });
});

describe('decodeMarketHeader', () => {
  it('reads the documented offsets (no ltv_buffer_bps in FeeConfig)', () => {
    const buf = new Uint8Array(MARKET_FIXED_SIZE);
    writeU64LE(buf, 0, DISC_MARKET_FIXED);
    buf[8] = 1; // version
    buf[9] = 6; // debt_mint_decimals
    buf[10] = 9; // collateral_mint_decimals
    const debtMint = new PublicKey('11111111111111111111111111111112');
    const collatMint = new PublicKey('11111111111111111111111111111113');
    writePk(buf, 16, debtMint);
    writePk(buf, 48, collatMint);
    writeU128LE(buf, 144, 0n);          // accumulated_protocol_fee_shares
    writeU64LE(buf, 160, 50n);          // order_sequence_number
    writeU64LE(buf, 168, 10n);          // matched_loan_sequence
    writeU32LE(buf, 188, NIL_INDEX);    // asks_root_index (empty)
    writeU32LE(buf, 196, NIL_INDEX);    // claimed_seats_root_index
    writeU32LE(buf, 200, NIL_INDEX);    // matched_loans_root_index
    writeU32LE(buf, 208, 3);            // position_count
    // FeeConfig @212: protocol_fee_bps_floor(0) origination(2) curator_split(4)
    // liquidation_keeper(6) liquidation_protocol(8) [pad 10] grace_period(16).
    writeU16LE(buf, 212 + 0, 50);       // FeeConfig.protocol_fee_bps_floor
    writeU16LE(buf, 212 + 2, 100);      // FeeConfig.origination_bps
    writeU16LE(buf, 212 + 4, 1_000);    // FeeConfig.curator_split_bps
    writeU16LE(buf, 212 + 6, 20);       // FeeConfig.liquidation_keeper_bps
    writeU16LE(buf, 212 + 8, 30);       // FeeConfig.liquidation_protocol_bps
    writeU32LE(buf, 212 + 16, 86_400);  // FeeConfig.grace_period_seconds
    const admin = new PublicKey('11111111111111111111111111111114');
    writePk(buf, 440, admin);
    buf[504] = 0; // is_paused = false

    const header = decodeMarketHeader(buf);
    expect(header.version).toBe(1);
    expect(header.debtMintDecimals).toBe(6);
    expect(header.collateralMintDecimals).toBe(9);
    expect(header.debtMint.equals(debtMint)).toBe(true);
    expect(header.collateralMint.equals(collatMint)).toBe(true);
    expect(header.orderSequenceNumber).toBe(50n);
    expect(header.matchedLoanSequence).toBe(10n);
    expect(header.positionCount).toBe(3);
    expect(header.feeConfig.protocolFeeBpsFloor).toBe(50);
    expect(header.feeConfig.originationBps).toBe(100);
    expect(header.feeConfig.curatorSplitBps).toBe(1_000);
    expect(header.feeConfig.liquidationKeeperBps).toBe(20);
    expect(header.feeConfig.liquidationProtocolBps).toBe(30);
    expect(header.feeConfig.gracePeriodSeconds).toBe(86_400);
    // `ltvBufferBps` was removed from FeeConfig in v1.
    expect('ltvBufferBps' in header.feeConfig).toBe(false);
    expect(header.admin.equals(admin)).toBe(true);
    expect(header.isPaused).toBe(false);
  });
});

describe('decodeGlobalVaultHeader', () => {
  it('reads mint, admin, lending_pool, tree roots, sub_vault_count (u16), is_paused', () => {
    const buf = new Uint8Array(GLOBAL_VAULT_FIXED_SIZE);
    writeU64LE(buf, 0, DISC_GLOBAL_VAULT);
    const mint = new PublicKey('11111111111111111111111111111112');
    const admin = new PublicKey('11111111111111111111111111111113');
    const lendingPool = new PublicKey('11111111111111111111111111111114');
    writePk(buf, 8, mint);
    writePk(buf, 40, admin);
    writePk(buf, 168, lendingPool);  // lending_pool (the bank the vault is keyed to)
    writeU32LE(buf, 200, NIL_INDEX); // sub_vaults_root
    writeU32LE(buf, 204, NIL_INDEX); // claimed_seats_root
    writeU32LE(buf, 208, NIL_INDEX); // market_orders_root
    writeU32LE(buf, 220, 1024);       // num_bytes_allocated
    writeU16LE(buf, 224, 5);          // sub_vault_count (u16)
    buf[227] = 1;                     // version
    buf[304] = 1;                     // is_paused
    writeU16LE(buf, 306, 6);          // next_sub_vault_id
    const h = decodeGlobalVaultHeader(buf);
    expect(h.mint.equals(mint)).toBe(true);
    expect(h.globalVaultAdmin.equals(admin)).toBe(true);
    expect(h.lendingPool.equals(lendingPool)).toBe(true);
    expect(h.subVaultCount).toBe(5);
    expect(h.numBytesAllocated).toBe(1024);
    expect(h.version).toBe(1);
    expect(h.isPaused).toBe(true);
    expect(h.nextSubVaultId).toBe(6);
  });
});

/* ── Tree walker tests ──────────────────────────────────── */

describe('RB tree walkers', () => {
  // Build a 3-node tree:
  //         (root @ 0)         payload = 'A'  (key 50)
  //        /            \
  //   (left @ 160)     (right @ 320)
  //   payload = 'B'    payload = 'C'
  //   (key 25)         (key 75)
  //
  // Tree-key irrelevant for the walker — it just follows left/right/parent
  // pointers. We test pure traversal order.
  function buildTreeBytes(): Uint8Array {
    const stride = 160; // 16 header + 144 payload
    const dynamic = new Uint8Array(stride * 3);
    const dv = new DataView(dynamic.buffer);

    // Root node @ 0
    dv.setUint32(0 + 0, 160, true);          // left = node @160
    dv.setUint32(0 + 4, 320, true);          // right = node @320
    dv.setUint32(0 + 8, NIL_INDEX, true);    // parent = NIL
    dynamic[0 + 16] = 0x41;                  // payload[0] = 'A'

    // Left child @ 160
    dv.setUint32(160 + 0, NIL_INDEX, true);  // left = NIL
    dv.setUint32(160 + 4, NIL_INDEX, true);  // right = NIL
    dv.setUint32(160 + 8, 0, true);          // parent = 0
    dynamic[160 + 16] = 0x42;                // payload[0] = 'B'

    // Right child @ 320
    dv.setUint32(320 + 0, NIL_INDEX, true);  // left = NIL
    dv.setUint32(320 + 4, NIL_INDEX, true);  // right = NIL
    dv.setUint32(320 + 8, 0, true);          // parent = 0
    dynamic[320 + 16] = 0x43;                // payload[0] = 'C'

    return dynamic;
  }

  it('walkDescending visits largest → smallest by in-order Ord', () => {
    const dynamic = buildTreeBytes();
    const dv = new DataView(dynamic.buffer);
    const out: number[] = [];
    for (const { payload } of walkDescending(dv, 0, 144)) {
      out.push(payload.getUint8(0));
    }
    // Descending in-order traversal from the right subtree: C, A, B.
    expect(out).toEqual([0x43, 0x41, 0x42]);
  });

  it('walkAscending visits smallest → largest', () => {
    const dynamic = buildTreeBytes();
    const dv = new DataView(dynamic.buffer);
    const out: number[] = [];
    for (const { payload } of walkAscending(dv, 0, 144)) {
      out.push(payload.getUint8(0));
    }
    expect(out).toEqual([0x42, 0x41, 0x43]);
  });

  it('walkDescending on NIL root yields nothing', () => {
    const dynamic = new Uint8Array(0);
    const dv = new DataView(dynamic.buffer);
    expect([...walkDescending(dv, NIL_INDEX, 144)]).toEqual([]);
  });

  it('walkFreeList chases the +0 u32 pointer until NIL', () => {
    // Three free blocks: head @ 0 → 160 → 320 → NIL.
    const dynamic = new Uint8Array(160 * 3);
    const dv = new DataView(dynamic.buffer);
    dv.setUint32(0, 160, true);
    dv.setUint32(160, 320, true);
    dv.setUint32(320, NIL_INDEX, true);
    expect([...walkFreeList(dv, 0)]).toEqual([0, 160, 320]);
  });
});
