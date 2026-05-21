/**
 * create-global-config.ts — fires the `CreateGlobalConfig` ix (tag 28).
 *
 * Reads:
 *   .local/protocol.json   { programId, programDataPda, deployer }
 * Writes:
 *   .local/global-config.json {
 *     pda, protocolAdmin, signature
 *   }
 *
 * Idempotent: if `.local/global-config.json` already exists, we no-op.
 * If the PDA already exists on-chain (e.g. the file was lost) we rebuild
 * the JSON from chain state without resending the ix.
 */
import { PublicKey } from '@solana/web3.js';

import { decodeGlobalConfig } from '../src/accounts/globalConfig.js';
import { YDELTA_PROGRAM_ID } from '../src/constants.js';
import { createGlobalConfigInstruction } from '../src/instructions/index.js';
import { globalConfigPda } from '../src/pdas.js';
import {
  appendTxLog,
  loadConnection,
  loadSigner,
  loadProtocol,
  log,
  readJsonOptional,
  sendIxs,
  writeJson,
} from './_runner.js';

interface GlobalConfigDump {
  pda: string;
  protocolAdmin: string;
  signature?: string;
  alreadyExisted?: boolean;
}

async function main(): Promise<void> {
  const existing = readJsonOptional<GlobalConfigDump>('global-config.json');
  if (existing) {
    log(`[create-global-config] .local/global-config.json already present (pda=${existing.pda}); nothing to do`);
    return;
  }

  const conn = loadConnection();
  const signer = loadSigner();
  const protocol = loadProtocol();

  const programId = new PublicKey(protocol.programId);
  if (!programId.equals(YDELTA_PROGRAM_ID)) {
    throw new Error(
      `protocol.json programId ${programId.toBase58()} does not match the SDK's ` +
      `pinned YDELTA_PROGRAM_ID ${YDELTA_PROGRAM_ID.toBase58()}. ` +
      `Either rebuild the SDK or check you're targeting the right deploy.`,
    );
  }

  const [pda] = globalConfigPda();
  const onchain = await conn.getAccountInfo(pda);
  if (onchain) {
    const globalConfig = decodeGlobalConfig(onchain.data);
    log(`[create-global-config] PDA ${pda.toBase58()} already exists; recording without sending`);
    writeJson('global-config.json', {
      pda: pda.toBase58(),
      protocolAdmin: globalConfig.protocolAdmin.toBase58(),
      alreadyExisted: true,
    } satisfies GlobalConfigDump);
    return;
  }

  log(`[create-global-config] signer = ${signer.publicKey.toBase58()}`);
  log(`[create-global-config] global_config pda = ${pda.toBase58()}`);
  log(`[create-global-config] program_data = ${protocol.programDataPda}`);

  const ix = createGlobalConfigInstruction({
    payer: signer.publicKey,
    programData: new PublicKey(protocol.programDataPda),
  });
  const sig = await sendIxs(conn, signer, [ix]);
  log(`[create-global-config] signature = ${sig}`);

  writeJson('global-config.json', {
    pda: pda.toBase58(),
    protocolAdmin: signer.publicKey.toBase58(),
    signature: sig,
  } satisfies GlobalConfigDump);
  appendTxLog({
    script: 'create-global-config',
    signatures: [sig],
    summary: {
      pda: pda.toBase58(),
      protocolAdmin: signer.publicKey.toBase58(),
    },
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
