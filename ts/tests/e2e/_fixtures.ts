/**
 * Shared pubkey constants from the on-chain marginfi v0.1.8 fixtures loaded
 * by `_bankrun.ts`. These match `programs/ydelta/tests/fixtures/*.json` —
 * dumped from mainnet at a known slot.
 */
import { PublicKey } from '@solana/web3.js';

export const MARGINFI_PROGRAM_ID = new PublicKey(
  'MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA',
);
export const MARGINFI_GROUP = new PublicKey('4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8');
export const USDC_BANK = new PublicKey('2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB');
export const SOL_BANK = new PublicKey('CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh');
export const USDC_MINT = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');
export const WSOL_MINT = new PublicKey('So11111111111111111111111111111111111111112');
export const USDC_LIQUIDITY_VAULT = new PublicKey(
  '7jaiZR5Sk8hdYN9MxTpczTcwbWpb5WEoxSANuUwveuat',
);
export const SOL_LIQUIDITY_VAULT = new PublicKey(
  '2eicbpitfJXDwqCuFAmPgDP7t2oUotnAzbGzRKLMgSLe',
);
export const USDC_ORACLE = new PublicKey('Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX');
export const SOL_ORACLE = new PublicKey('4Hmd6PdjVA9auCoScE12iaBogfwS4ZXQ6VZoBeqanwWW');
export const SPL_TOKEN_PROGRAM_ID = new PublicKey(
  'TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA',
);
