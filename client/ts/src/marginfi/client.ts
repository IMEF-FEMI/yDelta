import { Connection, PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import {
  Bank,
  MARGINFI_IDL,
  OraclePrice,
  PythPushFeedIdMap,
} from '@mrgnlabs/marginfi-client-v2';

/** Read-only marginfi client wrapper. */

export const MARGINFI_PROGRAM_ID = new PublicKey('MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA');

export class MarginfiReader {
  private banks = new Map<string, Bank>();
  private prices = new Map<string, OraclePrice>();
  private feedMap?: PythPushFeedIdMap;

  constructor(public readonly connection: Connection) {}

  /** Fetch + parse a single marginfi `Bank`. Cached by pubkey. */
  async loadBank(bank: PublicKey, force = false): Promise<Bank> {
    const key = bank.toBase58();
    if (!force) {
      const cached = this.banks.get(key);
      if (cached) return cached;
    }
    const info = await this.connection.getAccountInfo(bank);
    if (!info) throw new Error(`marginfi bank ${key} not found on ${this.connection.rpcEndpoint}`);
    if (!this.feedMap) this.feedMap = new Map();
    const parsed = Bank.fromBuffer(bank, info.data, MARGINFI_IDL, this.feedMap);
    this.banks.set(key, parsed);
    return parsed;
  }

  /** Force a re-fetch of `bank`. */
  async refresh(bank: PublicKey): Promise<Bank> {
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
