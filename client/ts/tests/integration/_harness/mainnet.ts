import { PublicKey } from '@solana/web3.js';

/** Marginfi v0.1.8 mainnet program id. The `.so` checked into
 *  `client/ts/tests/integration/fixtures/marginfi.so` (copied from
 *  `programs/ydelta/tests/fixtures/`) is loaded at this address. */
export const MARGINFI_PROGRAM_ID = new PublicKey('MFv2hWf31Z9kbCa1snEPYctwafyhdvnV7FZnsebVacA');

/** ydelta-test-harness program id (test-only wrapper around the marginfi adapter). */
export const YDELTA_TEST_HARNESS_PROGRAM_ID = new PublicKey('Fm3cEUk47y2NWZhDzpL8mRVTg8JY1QCwbzCPjGLgvty9');

/** Mainnet account pubkeys that we replay into bankrun. Mirrors
 *  `programs/ydelta/tests/test_utils/marginfi_fixture.rs::mainnet`. */
export const MARGINFI_GROUP = new PublicKey('4qp6Fx6tnZkY5Wropq9wUYgtFxXKwE6viZxFHg3rdAG8');

export const USDC_BANK = new PublicKey('2s37akK2eyBbp8DZgCm7RtsaEz8eJP3Nxd4urLHQv7yB');
export const USDC_MINT = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');
export const USDC_LIQUIDITY_VAULT = new PublicKey('7jaiZR5Sk8hdYN9MxTpczTcwbWpb5WEoxSANuUwveuat');
export const USDC_ORACLE = new PublicKey('Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX');

export const SOL_BANK = new PublicKey('CCKtUs6Cgwo4aaQUmBPmyoApH2gUDErxNZCAntD6LYGh');
export const WSOL_MINT = new PublicKey('So11111111111111111111111111111111111111112');
export const SOL_LIQUIDITY_VAULT = new PublicKey('2eicbpitfJXDwqCuFAmPgDP7t2oUotnAzbGzRKLMgSLe');
export const SOL_ORACLE = new PublicKey('4Hmd6PdjVA9auCoScE12iaBogfwS4ZXQ6VZoBeqanwWW');

/** Marginfi's `[b"liquidity_vault_auth", bank]` PDA derivation. */
export function bankLiquidityVaultAuthority(bank: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('liquidity_vault_auth'), bank.toBuffer()],
    MARGINFI_PROGRAM_ID,
  )[0];
}

/** All fixture filenames + the pubkeys they should land at. */
export const MAINNET_ACCOUNT_FIXTURES: ReadonlyArray<{ filename: string; pubkey: PublicKey }> = [
  { filename: 'marginfi_group.json', pubkey: MARGINFI_GROUP },
  { filename: 'marginfi_usdc_bank.json', pubkey: USDC_BANK },
  { filename: 'usdc_mint.json', pubkey: USDC_MINT },
  { filename: 'marginfi_usdc_liquidity_vault.json', pubkey: USDC_LIQUIDITY_VAULT },
  { filename: 'usdc_oracle.json', pubkey: USDC_ORACLE },
  { filename: 'marginfi_sol_bank.json', pubkey: SOL_BANK },
  { filename: 'wsol_mint.json', pubkey: WSOL_MINT },
  { filename: 'marginfi_sol_liquidity_vault.json', pubkey: SOL_LIQUIDITY_VAULT },
  { filename: 'sol_oracle.json', pubkey: SOL_ORACLE },
];
