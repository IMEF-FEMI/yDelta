import { Connection, PublicKey } from '@solana/web3.js';
import {
  UserAccountFixed,
  decodeUserAccountFixed,
  USER_ACCOUNT_FIXED_SIZE,
  VaultPosition,
  decodeVaultPosition,
  MarketPosition,
  decodeMarketPosition,
  UserLoanRef,
  decodeUserLoanRef,
} from './ydelta/accounts';
import { UserLoanRole } from './ydelta/types/enums';
import { USER_ACCOUNT_BLOCK_SIZE, isNotNil } from './utils/discriminator';
import { collectTree, findInTree } from './utils/redBlackTree';
import { userAccountPda } from './utils/pdas';

/** Wraps a `UserAccountFixed`: header decode + RB-tree access over vault positions, market positions, and open loans. */
export class UserAccount {
  readonly address: PublicKey;
  private _buffer: Buffer;
  private _header: UserAccountFixed;

  private constructor(address: PublicKey, buffer: Buffer) {
    this.address = address;
    this._buffer = buffer;
    this._header = decodeUserAccountFixed(buffer);
  }

  static async loadForOwner({
    connection,
    owner,
    programId,
  }: {
    connection: Connection;
    owner: PublicKey;
    programId?: PublicKey;
  }): Promise<UserAccount | null> {
    const [pda] = userAccountPda(owner, programId);
    const info = await connection.getAccountInfo(pda);
    if (!info) return null;
    return new UserAccount(pda, Buffer.from(info.data));
  }

  static loadFromBuffer({
    address,
    buffer,
  }: {
    address: PublicKey;
    buffer: Buffer;
  }): UserAccount {
    return new UserAccount(address, buffer);
  }

  async reload(connection: Connection): Promise<void> {
    const info = await connection.getAccountInfo(this.address);
    if (!info) throw new Error(`UserAccount ${this.address.toBase58()} not found`);
    this._buffer = Buffer.from(info.data);
    this._header = decodeUserAccountFixed(this._buffer);
  }

  header(): UserAccountFixed {
    return this._header;
  }

  private dynamic(): Buffer {
    return this._buffer.subarray(USER_ACCOUNT_FIXED_SIZE);
  }

  owner(): PublicKey {
    return this._header.owner;
  }

  /** All vault positions held by this user. */
  getVaultPositions(): VaultPosition[] {
    if (!isNotNil(this._header.vaultPositionsRootIndex)) return [];
    return collectTree(
      this.dynamic(),
      this._header.vaultPositionsRootIndex,
      USER_ACCOUNT_BLOCK_SIZE,
      decodeVaultPosition,
    );
  }

  /** O(log N) keyed lookup. The vault-positions tree is keyed by
   *  `(vault, profile_id)`: vault pubkey @ 0..32, profile_id @ 32. */
  getVaultPosition(vault: PublicKey, profileId: number): VaultPosition | undefined {
    if (!isNotNil(this._header.vaultPositionsRootIndex)) return undefined;
    const vaultBytes = vault.toBuffer();
    const result = findInTree(
      this.dynamic(),
      this._header.vaultPositionsRootIndex,
      USER_ACCOUNT_BLOCK_SIZE,
      decodeVaultPosition,
      (payload) => {
        const cmp = Buffer.compare(vaultBytes, payload.subarray(0, 32));
        if (cmp !== 0) return cmp;
        return profileId - payload.readUInt8(32);
      },
    );
    return result ?? undefined;
  }

  getMarketPositions(): MarketPosition[] {
    if (!isNotNil(this._header.marketPositionsRootIndex)) return [];
    return collectTree(
      this.dynamic(),
      this._header.marketPositionsRootIndex,
      USER_ACCOUNT_BLOCK_SIZE,
      decodeMarketPosition,
    );
  }

  /** O(log N) keyed lookup. Market-positions tree is keyed by market pubkey. */
  getMarketPosition(market: PublicKey): MarketPosition | undefined {
    if (!isNotNil(this._header.marketPositionsRootIndex)) return undefined;
    const marketBytes = market.toBuffer();
    const result = findInTree(
      this.dynamic(),
      this._header.marketPositionsRootIndex,
      USER_ACCOUNT_BLOCK_SIZE,
      decodeMarketPosition,
      (payload) => Buffer.compare(marketBytes, payload.subarray(0, 32)),
    );
    return result ?? undefined;
  }

  /** Returns refs, not full Loan accounts. */
  getOpenLoans(): UserLoanRef[] {
    if (!isNotNil(this._header.openLoansRootIndex)) return [];
    return collectTree(
      this.dynamic(),
      this._header.openLoansRootIndex,
      USER_ACCOUNT_BLOCK_SIZE,
      decodeUserLoanRef,
    );
  }

  /** Open loans where the user is the borrower. */
  getActiveLoansAsBorrower(): UserLoanRef[] {
    return this.getOpenLoans().filter((l) => l.role === UserLoanRole.Borrower);
  }

  getActiveLoansAsLender(): UserLoanRef[] {
    return this.getOpenLoans().filter((l) => l.role === UserLoanRole.Lender);
  }
}
