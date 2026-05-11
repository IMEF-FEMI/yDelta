import { Connection, PublicKey } from '@solana/web3.js';
import BN from 'bn.js';
import {
  GlobalVaultFixed,
  decodeGlobalVaultFixed,
  GLOBAL_VAULT_FIXED_SIZE,
  RiskProfile,
  decodeRiskProfile,
  idleAtoms as riskProfileIdleAtoms,
  withdrawableAtoms as riskProfileWithdrawableAtoms,
  weightedAvgRateBps,
  RiskProfileDepositorSeat,
  decodeRiskProfileDepositorSeat,
  RiskProfileOrderRef,
  decodeRiskProfileOrderRef,
} from './ydelta/accounts';
import {
  RISK_PROFILE_BLOCK_SIZE,
  VAULT_NODE_BLOCK_SIZE,
  isNotNil,
} from './utils/discriminator';
import { collectTree, findInTree } from './utils/redBlackTree';
import { globalVaultPda } from './utils/pdas';

/** Wraps a `GlobalVaultFixed` account. */
export class Vault {
  readonly address: PublicKey;
  private _buffer: Buffer;
  private _header: GlobalVaultFixed;
  private _profileCache = new Map<number, RiskProfile | null>();

  private constructor(address: PublicKey, buffer: Buffer) {
    this.address = address;
    this._buffer = buffer;
    this._header = decodeGlobalVaultFixed(buffer);
  }

  static async loadForMint({
    connection,
    mint,
    programId,
  }: {
    connection: Connection;
    mint: PublicKey;
    programId?: PublicKey;
  }): Promise<Vault | null> {
    const [pda] = globalVaultPda(mint, programId);
    const info = await connection.getAccountInfo(pda);
    if (!info) return null;
    return new Vault(pda, Buffer.from(info.data));
  }

  static async loadFromAddress({
    connection,
    address,
  }: {
    connection: Connection;
    address: PublicKey;
  }): Promise<Vault | null> {
    const info = await connection.getAccountInfo(address);
    if (!info) return null;
    return new Vault(address, Buffer.from(info.data));
  }

  static loadFromBuffer({
    address,
    buffer,
  }: {
    address: PublicKey;
    buffer: Buffer;
  }): Vault {
    return new Vault(address, buffer);
  }

  async reload(connection: Connection): Promise<void> {
    const info = await connection.getAccountInfo(this.address);
    if (!info) throw new Error(`Vault ${this.address.toBase58()} not found`);
    this._buffer = Buffer.from(info.data);
    this._header = decodeGlobalVaultFixed(this._buffer);
    this._profileCache.clear();
  }

  header(): GlobalVaultFixed {
    return this._header;
  }

  private dynamic(): Buffer {
    return this._buffer.subarray(GLOBAL_VAULT_FIXED_SIZE);
  }

  mint(): PublicKey {
    return this._header.mint;
  }

  admin(): PublicKey {
    return this._header.globalVaultAdmin;
  }

  pendingAdmin(): PublicKey {
    return this._header.pendingGlobalVaultAdmin;
  }

  lendingPool(): PublicKey {
    return this._header.lendingPool;
  }

  /** O(log N) profile lookup with caching. Descends the `risk_profiles`
   *  RB-tree using `profile_id` as the comparator key — matches the on-chain
   *  `Ord` impl on `RiskProfile` (vault.rs), which sorts by `profile_id`
   *  ascending. */
  getRiskProfile(profileId: number): RiskProfile | null {
    if (this._profileCache.has(profileId)) {
      return this._profileCache.get(profileId)!;
    }
    if (!isNotNil(this._header.riskProfilesRootIndex)) {
      this._profileCache.set(profileId, null);
      return null;
    }
    // `profile_id` is the first byte of the RiskProfile payload.
    const found = findInTree(
      this.dynamic(),
      this._header.riskProfilesRootIndex,
      RISK_PROFILE_BLOCK_SIZE,
      decodeRiskProfile,
      (payload) => profileId - payload.readUInt8(0),
    );
    this._profileCache.set(profileId, found);
    return found;
  }

  /** Eagerly materialise every risk-profile in the vault. ADMIN/DASHBOARD ONLY.
   *  Pre-loading the full tree wastes CU on a hot path and a large vault can
   *  hold many profiles — never call this from origination, settlement,
   *  liquidation, or any keeper loop. Use `getRiskProfile(profileId)` for
   *  O(log N) keyed lookups in normal flows. */
  getAllRiskProfilesUnsafeBulk(): RiskProfile[] {
    if (!isNotNil(this._header.riskProfilesRootIndex)) return [];
    return collectTree(
      this.dynamic(),
      this._header.riskProfilesRootIndex,
      RISK_PROFILE_BLOCK_SIZE,
      decodeRiskProfile,
    );
  }

  /** @deprecated Renamed to `getAllRiskProfilesUnsafeBulk()` to make the
   *  performance hazard explicit. This shim will be removed in 0.2. */
  getRiskProfiles(): RiskProfile[] {
    return this.getAllRiskProfilesUnsafeBulk();
  }

  /** All depositor seats. Vault-level dashboard surface; for a single owner
   *  prefer `getDepositorSeat(owner, profileId)`. */
  getDepositorSeats(): RiskProfileDepositorSeat[] {
    if (!isNotNil(this._header.claimedSeatsRootIndex)) return [];
    return collectTree(
      this.dynamic(),
      this._header.claimedSeatsRootIndex,
      VAULT_NODE_BLOCK_SIZE,
      decodeRiskProfileDepositorSeat,
    );
  }

  getDepositorSeat(owner: PublicKey, profileId: number): RiskProfileDepositorSeat | null {
    if (!isNotNil(this._header.claimedSeatsRootIndex)) return null;
    // Composite key `(owner, profile_id)` — owner first, then profile_id.
    // owner is at payload offset 0..32; profile_id at offset 32.
    const ownerBytes = owner.toBuffer();
    return findInTree(
      this.dynamic(),
      this._header.claimedSeatsRootIndex,
      VAULT_NODE_BLOCK_SIZE,
      decodeRiskProfileDepositorSeat,
      (payload) => {
        const ownerCmp = Buffer.compare(ownerBytes, payload.subarray(0, 32));
        if (ownerCmp !== 0) return ownerCmp;
        return profileId - payload.readUInt8(32);
      },
    );
  }

  /** All live `(market, profile_id)` order refs. */
  getOrderRefs(): RiskProfileOrderRef[] {
    if (!isNotNil(this._header.marketOrdersRootIndex)) return [];
    return collectTree(
      this.dynamic(),
      this._header.marketOrdersRootIndex,
      VAULT_NODE_BLOCK_SIZE,
      decodeRiskProfileOrderRef,
    );
  }

  getOrderRef(market: PublicKey, profileId: number): RiskProfileOrderRef | null {
    if (!isNotNil(this._header.marketOrdersRootIndex)) return null;
    // Composite key `(market, profile_id)`. market 0..32, profile_id @ 32.
    const marketBytes = market.toBuffer();
    return findInTree(
      this.dynamic(),
      this._header.marketOrdersRootIndex,
      VAULT_NODE_BLOCK_SIZE,
      decodeRiskProfileOrderRef,
      (payload) => {
        const marketCmp = Buffer.compare(marketBytes, payload.subarray(0, 32));
        if (marketCmp !== 0) return marketCmp;
        return profileId - payload.readUInt8(32);
      },
    );
  }

  /** Principal available to back NEW orders (matches the on-chain
   *  matching gate: `total_principal_atoms - deployed - encumbered`).
   *  Use this when sizing a vault order. */
  idleAtoms(profileId: number): BN {
    const p = this.getRiskProfile(profileId);
    if (!p) return new BN(0);
    return riskProfileIdleAtoms(p);
  }

  /** Atoms withdrawable by depositors (assets-side; includes accrued yield).
   *  Use this for UI redemption previews, NOT for order sizing. */
  withdrawableAtoms(profileId: number): BN {
    const p = this.getRiskProfile(profileId);
    if (!p) return new BN(0);
    return riskProfileWithdrawableAtoms(p);
  }

  deployedAtoms(profileId: number): BN {
    return this.getRiskProfile(profileId)?.deployedPrincipalAtoms ?? new BN(0);
  }

  /** Coarse weighted-average APY across open loans, in bps. 0 when nothing deployed. */
  computeProfileApySnapshotBps(profileId: number): number {
    const p = this.getRiskProfile(profileId);
    return p ? weightedAvgRateBps(p) : 0;
  }
}
