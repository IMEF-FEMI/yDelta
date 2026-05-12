import { Connection, PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import { OraclePrice } from '@mrgnlabs/marginfi-client-v2';

import { BankView, loadBankView } from './bank-view';

/** Read-only marginfi client wrapper. */

export const MARGINFI_PROGRAM_ID = new PublicKey('MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA');

/**
 * Read-only cache + thin wrapper around the raw-bytes `BankView`
 * decoder. Use this instead of constructing `MarginfiClient` from
 * `@mrgnlabs/marginfi-client-v2` — the package's `Bank.fromBuffer` is
 * broken on mainnet banks (case-mismatch + WrappedI80F48 shape issues
 * in v6.4.1; see `bank-view.ts` for the writeup).
 */
export class MarginfiReader {
  private banks = new Map<string, BankView>();
  private prices = new Map<string, OraclePrice>();

  constructor(public readonly connection: Connection) {}

  /** Fetch + parse a single marginfi `Bank`. Cached by pubkey. */
  async loadBank(bank: PublicKey, force = false): Promise<BankView> {
    const key = bank.toBase58();
    if (!force) {
      const cached = this.banks.get(key);
      if (cached) return cached;
    }
    const parsed = await loadBankView(this.connection, bank);
    this.banks.set(key, parsed);
    return parsed;
  }

  /** Force a re-fetch of `bank`. */
  async refresh(bank: PublicKey): Promise<BankView> {
    return this.loadBank(bank, /*force=*/ true);
  }

  /** Dev-only stub: throws so callers don't silently hit a non-wired path.
   *  Replace with `MarginfiClient.fetchPriceInfo` when real oracle reading is
   *  needed; until then wire prices explicitly via `setOraclePrice` /
   *  `setSpotPrice`. */
  async loadOraclePrice(bank: PublicKey): Promise<OraclePrice> {
    const parsed = await this.loadBank(bank);
    const oracleKey = parsed.config.oracleKeys[0];
    if (!oracleKey || oracleKey.equals(PublicKey.default)) {
      throw new Error(`bank ${bank.toBase58()} has no oracle_keys[0]`);
    }
    const cached = this.prices.get(bank.toBase58());
    if (cached) return cached;
    throw new Error(
      `loadOraclePrice is a dev stub — call setOraclePrice(bank, price) or fetch via ` +
        `MarginfiClient.fetchPriceInfo() and feed it in (bank ${bank.toBase58()}, ` +
        `oracle ${oracleKey.toBase58()})`,
    );
  }

  /** Wire an externally-supplied `OraclePrice` for a bank. */
  setOraclePrice(bank: PublicKey, price: OraclePrice): void {
    this.prices.set(bank.toBase58(), price);
  }

  /** Dev-time price wiring: `confidence: 0` bypasses the 2.12σ Pyth / 1.96σ Switchboard confidence-interval rejection — dev/test only. */
  setSpotPrice(bank: PublicKey, usdPrice: number): void {
    const bn = new BigNumber(usdPrice);
    const oraclePrice: OraclePrice = {
      priceRealtime: { price: bn, confidence: new BigNumber(0), lowestPrice: bn, highestPrice: bn },
      priceWeighted: { price: bn, confidence: new BigNumber(0), lowestPrice: bn, highestPrice: bn },
      timestamp: new BigNumber(Math.floor(Date.now() / 1000)),
    } as OraclePrice;
    this.setOraclePrice(bank, oraclePrice);
  }

  /** Return the cached `OraclePrice` for `bank` or `undefined`. */
  oraclePriceFor(bank: PublicKey): OraclePrice | undefined {
    return this.prices.get(bank.toBase58());
  }
}

// Re-export the lean snapshot shape so callers can keep using it (kept
// as a type alias on top of BankView for backwards-compat with the
// already-shipped SWB-crank flow).
export type BankSnapshot = {
  address: PublicKey;
  mint: PublicKey;
  liquidityVault: PublicKey;
  liquidityVaultAuthorityBump: number;
  oracleSetup: number;
  oracleKeys: PublicKey[];
  oracleMaxAge: number;
};

/** Lean snapshot — same surface the SWB-crank path uses. Forwards to
 *  the full `BankView` decoder. */
export async function loadBankSnapshot(
  connection: Connection,
  address: PublicKey,
): Promise<BankSnapshot> {
  const v = await loadBankView(connection, address);
  return {
    address: v.address,
    mint: v.mint,
    liquidityVault: v.liquidityVault,
    liquidityVaultAuthorityBump: v.liquidityVaultAuthorityBump,
    oracleSetup: v.config.oracleSetup,
    oracleKeys: v.config.oracleKeys,
    oracleMaxAge: v.config.oracleMaxAge,
  };
}

export function bankSnapshotFromView(v: BankView): BankSnapshot {
  return {
    address: v.address,
    mint: v.mint,
    liquidityVault: v.liquidityVault,
    liquidityVaultAuthorityBump: v.liquidityVaultAuthorityBump,
    oracleSetup: v.config.oracleSetup,
    oracleKeys: v.config.oracleKeys,
    oracleMaxAge: v.config.oracleMaxAge,
  };
}
