import { PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import { Market } from '../market';
import { MarginfiReader } from './client';
import { getBankLiquidity, getBankRates } from './bank';

export type MarketBankRelationship = {
  /** Atoms the bank holds in total (deposits). */
  bankTotalAssetAtoms: BigNumber;
  /** Atoms borrowed across all bank consumers. */
  bankTotalLiabilityAtoms: BigNumber;
  /** Atoms in the yDelta lender-side integration account (the market's USDC
   *  supply that's been routed through marginfi). */
  ydeltaSuppliedAtoms: BigNumber;
  /** yDelta's share of bank assets, in [0,1]. */
  ydeltaSupplyShare: number;
  /** Atoms in the yDelta borrower-side integration account (P2Pool liabilities). */
  ydeltaBorrowedAtoms: BigNumber;
  /** yDelta's share of bank borrows, in [0,1]. */
  ydeltaBorrowShare: number;
  /** Current marginfi supply APY. */
  marginfiSupplyApy: number;
  /** Current marginfi borrow APR. */
  marginfiBorrowApr: number;
};

/** Compare a yDelta market's footprint inside its underlying marginfi bank.
 *  Powers "what % of marginfi USDC is yDelta?" and "is yDelta a meaningful
 *  source of liquidity?" displays. */
export async function getMarketBankRelationship({
  reader,
  market,
  bank,
}: {
  reader: MarginfiReader;
  market: Market;
  bank: PublicKey;
}): Promise<MarketBankRelationship> {
  const liquidity = await getBankLiquidity(reader, bank);
  const rates = await getBankRates(reader, bank);
  const bankParsed = await reader.loadBank(bank);

  const lenderIntegrationKey = market.header().lenderIntegrationAccount;
  const borrowerIntegrationKey = market.header().borrowerIntegrationAccount;

  // To compute yDelta's share, we'd ideally read the marginfi-account state at
  // these PDAs and pluck the (assetShares, liabilityShares) for the requested
  // bank, then convert via `bank.getAssetQuantity` / `getLiabilityQuantity`.
  // The MarginfiAccount parser lives in the SDK — we fetch + parse here.
  const [lenderAcc, borrowerAcc] = await Promise.all([
    reader.connection.getAccountInfo(lenderIntegrationKey),
    reader.connection.getAccountInfo(borrowerIntegrationKey),
  ]);

  let suppliedAtoms = new BigNumber(0);
  let borrowedAtoms = new BigNumber(0);
  if (lenderAcc) {
    const shares = readBalanceShares(lenderAcc.data, bank, 'asset');
    if (shares) suppliedAtoms = bankParsed.getAssetQuantity(shares);
  }
  if (borrowerAcc) {
    const shares = readBalanceShares(borrowerAcc.data, bank, 'liability');
    if (shares) borrowedAtoms = bankParsed.getLiabilityQuantity(shares);
  }

  return {
    bankTotalAssetAtoms: liquidity.totalAssetAtoms,
    bankTotalLiabilityAtoms: liquidity.totalLiabilityAtoms,
    ydeltaSuppliedAtoms: suppliedAtoms,
    ydeltaSupplyShare: liquidity.totalAssetAtoms.isZero()
      ? 0
      : suppliedAtoms.div(liquidity.totalAssetAtoms).toNumber(),
    ydeltaBorrowedAtoms: borrowedAtoms,
    ydeltaBorrowShare: liquidity.totalLiabilityAtoms.isZero()
      ? 0
      : borrowedAtoms.div(liquidity.totalLiabilityAtoms).toNumber(),
    marginfiSupplyApy: rates.supplyApy,
    marginfiBorrowApr: rates.borrowApr,
  };
}

/** Walk a marginfi `MarginfiAccount.balances[0..15]` for an active entry on
 *  `bank` and return the I80F48 asset/liability share quantity, or `null` if
 *  no match.
 *
 *  Body layout (104B per Balance entry):
 *     0     active u8
 *     1..33 bank_pk Pubkey
 *    33     bank_asset_tag u8
 *    34..36 tag u16
 *    36..40 _pad0
 *    40..56 asset_shares (WrappedI80F48, 16B LE)
 *    56..72 liability_shares (WrappedI80F48, 16B LE)
 *    72..88 emissions_outstanding
 *    88..96 last_update u64
 *    96..104 _padding */
function readBalanceShares(
  accData: Uint8Array,
  bank: PublicKey,
  kind: 'asset' | 'liability',
): BigNumber | null {
  const ANCHOR_HEADER = 8;
  const BALANCES_OFFSET = ANCHOR_HEADER + 32 + 32; // anchor + group + authority
  const BALANCE_SIZE = 104;
  const MAX_BALANCES = 16;
  for (let i = 0; i < MAX_BALANCES; i++) {
    const off = BALANCES_OFFSET + i * BALANCE_SIZE;
    if (off + BALANCE_SIZE > accData.length) break;
    if (accData[off] === 0) continue;
    const bankKey = new PublicKey(accData.slice(off + 1, off + 33));
    if (!bankKey.equals(bank)) continue;
    const sharesOff = kind === 'asset' ? off + 40 : off + 56;
    return i80f48ToBigNumber(accData.slice(sharesOff, sharesOff + 16));
  }
  return null;
}

/** Parse a 16-byte little-endian I80F48 (i128-encoded fixed-point) into a
 *  BigNumber. Mirrors the on-chain `wrapped_i80f48_to_u128` adapter in
 *  `programs/ydelta/src/protocol/marginfi.rs`: negative wire values are
 *  clamped to 0 (a transiently-negative WrappedI80F48 in marginfi state is
 *  treated as a degenerate zero). Without this clamp, sign-extension would
 *  let a downstream `getAssetQuantity` multiplication produce a non-physical
 *  negative atom count. */
function i80f48ToBigNumber(bytes: Uint8Array): BigNumber {
  let acc = 0n;
  for (let i = 15; i >= 0; i--) {
    acc = (acc << 8n) | BigInt(bytes[i]);
  }
  // Sign-extend if high bit of byte 15 is set.
  if (bytes[15] & 0x80) {
    acc -= 1n << 128n;
  }
  // Clamp negatives — matches on-chain semantics.
  if (acc < 0n) return new BigNumber(0);
  const scale = 1n << 48n;
  const intPart = acc / scale;
  const remainder = acc % scale;
  return new BigNumber(intPart.toString()).plus(
    new BigNumber(remainder.toString()).div(new BigNumber(scale.toString())),
  );
}
