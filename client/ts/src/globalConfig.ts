import { Connection, PublicKey } from '@solana/web3.js';
import {
  GlobalConfig as GlobalConfigHeader,
  decodeGlobalConfig,
} from './ydelta/accounts';
import { globalConfigPda } from './utils/pdas';

/** Singleton config account: protocol admin keys and global pause state. */
export class GlobalConfig {
  readonly address: PublicKey;
  private _buffer: Buffer;
  private _data: GlobalConfigHeader;

  private constructor(address: PublicKey, buffer: Buffer) {
    this.address = address;
    this._buffer = buffer;
    this._data = decodeGlobalConfig(buffer);
  }

  static async load({
    connection,
    programId,
  }: {
    connection: Connection;
    programId?: PublicKey;
  }): Promise<GlobalConfig | null> {
    const [pda] = globalConfigPda(programId);
    const info = await connection.getAccountInfo(pda);
    if (!info) return null;
    return new GlobalConfig(pda, Buffer.from(info.data));
  }

  static loadFromBuffer({
    address,
    buffer,
  }: {
    address: PublicKey;
    buffer: Buffer;
  }): GlobalConfig {
    return new GlobalConfig(address, buffer);
  }

  async reload(connection: Connection): Promise<void> {
    const info = await connection.getAccountInfo(this.address);
    if (!info) throw new Error('GlobalConfig not found');
    this._buffer = Buffer.from(info.data);
    this._data = decodeGlobalConfig(this._buffer);
  }

  /** Decoded header. */
  data(): GlobalConfigHeader {
    return this._data;
  }

  /** Current protocol admin pubkey. */
  protocolAdmin(): PublicKey {
    return this._data.protocolAdmin;
  }

  /** Two-step admin transfer staging slot; `default` until a transfer is initiated. */
  pendingProtocolAdmin(): PublicKey {
    return this._data.pendingProtocolAdmin;
  }

  /** Global pause flag — blocks state-mutating ixs across all markets and vaults. */
  isPaused(): boolean {
    return this._data.isPaused;
  }
}
