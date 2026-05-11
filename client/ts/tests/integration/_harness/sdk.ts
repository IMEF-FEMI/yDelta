import { Connection } from '@solana/web3.js';
import { BankrunProvider } from 'anchor-bankrun';
import { ProgramTestContext } from 'solana-bankrun';
import { YdeltaClient } from '../../../src/client';

/** A bankrun-backed YdeltaClient. The `connection` carried here is the
 *  BankrunProvider's polyfill, which routes RPC calls through `BanksClient`. */
export function bankrunYdeltaClient(context: ProgramTestContext): {
  provider: BankrunProvider;
  client: YdeltaClient;
  connection: Connection;
} {
  const provider = new BankrunProvider(context);
  const connection = provider.connection as unknown as Connection;
  return {
    provider,
    connection,
    client: new YdeltaClient({ connection }),
  };
}
