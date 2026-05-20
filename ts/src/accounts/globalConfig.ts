/**
 * `GlobalConfig` — 128-byte singleton at `[b"global_config"]`. Holds the
 * protocol-wide admin and global pause flag.
 *
 * Layout (bytemuck `#[repr(C)]`):
 *   @0   8   u64    discriminator (= 0x796465_6C7461_4763)
 *   @8   32  Pubkey protocol_admin
 *   @40  32  Pubkey pending_protocol_admin
 *   @72  1   u8     is_paused
 *   @73  7   u8[7]  _padding
 *   @80  48  u64[6] _reserved
 */
import { PublicKey } from '@solana/web3.js';

import { readPubkey, readU64, readU8, view } from './_read.js';

export const GLOBAL_CONFIG_SIZE = 128;
export const GLOBAL_CONFIG_DISCRIMINANT = 0x79_64_65_6c_74_61_47_63n; // "ydeltaGc"

export interface GlobalConfig {
  protocolAdmin: PublicKey;
  pendingProtocolAdmin: PublicKey;
  isPaused: boolean;
}

export function decodeGlobalConfig(data: Uint8Array | Buffer): GlobalConfig {
  if (data.byteLength < GLOBAL_CONFIG_SIZE) {
    throw new RangeError(
      `decodeGlobalConfig: account too small (${data.byteLength} < ${GLOBAL_CONFIG_SIZE})`,
    );
  }
  const dv = view(data);
  const disc = readU64(dv, 0);
  if (disc !== GLOBAL_CONFIG_DISCRIMINANT) {
    throw new Error(
      `decodeGlobalConfig: bad discriminator 0x${disc.toString(16)} (expected 0x${GLOBAL_CONFIG_DISCRIMINANT.toString(16)})`,
    );
  }
  return {
    protocolAdmin: readPubkey(dv, 8),
    pendingProtocolAdmin: readPubkey(dv, 40),
    isPaused: readU8(dv, 72) !== 0,
  };
}
