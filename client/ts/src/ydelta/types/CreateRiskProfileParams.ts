import * as beet from '@metaplex-foundation/beet';
import { publicKey as beetPublicKey } from '@metaplex-foundation/beet-solana';
import { PublicKey } from '@solana/web3.js';

export type CreateRiskProfileParams = {
  profileId: number;
  curator: PublicKey;
  maxLtvBps: number;
  maxTermSeconds: number;
  allowedMarketMax: number;
};

export const createRiskProfileParamsBeet = new beet.BeetArgsStruct<CreateRiskProfileParams>(
  [
    ['profileId', beet.u8],
    ['curator', beetPublicKey],
    ['maxLtvBps', beet.u16],
    ['maxTermSeconds', beet.u32],
    ['allowedMarketMax', beet.u8],
  ],
  'CreateRiskProfileParams',
);
