import { PublicKey } from '@solana/web3.js';
import {
  GLOBAL_CONFIG_DISCRIMINANT,
  readU64Le,
} from '../../utils/discriminator';

export const GLOBAL_CONFIG_SIZE = 128;

export type GlobalConfig = {
  discriminator: bigint;
  protocolAdmin: PublicKey;
  pendingProtocolAdmin: PublicKey;
  isPaused: boolean;
};

export function decodeGlobalConfig(buf: Buffer): GlobalConfig {
  if (buf.length < GLOBAL_CONFIG_SIZE) {
    throw new Error(`GlobalConfig buffer too small: ${buf.length} < ${GLOBAL_CONFIG_SIZE}`);
  }
  const discriminator = readU64Le(buf, 0);
  if (discriminator !== GLOBAL_CONFIG_DISCRIMINANT) {
    throw new Error(
      `GlobalConfig discriminator mismatch: 0x${discriminator.toString(16)} != 0x${GLOBAL_CONFIG_DISCRIMINANT.toString(16)}`,
    );
  }
  return {
    discriminator,
    protocolAdmin: new PublicKey(buf.subarray(8, 40)),
    pendingProtocolAdmin: new PublicKey(buf.subarray(40, 72)),
    isPaused: buf.readUInt8(72) !== 0,
  };
}
