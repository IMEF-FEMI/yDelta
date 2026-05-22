import 'dotenv/config';
import { Connection, PublicKey } from '@solana/web3.js';
import {
  fetchVault,
  decodeBank,
  decodeMarginfiAccount,
  findActiveBalance,
  globalVaultPda,
  globalVaultIntegrationAccountPda,
  globalVaultStagingPda,
  profileBalancesUi,
  riskProfileUi,
} from '../src/index.js';

const USDC = new PublicKey('EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v');
const DEC = 6;
const ui = (atoms: bigint) => (Number(atoms) / 10 ** DEC).toFixed(6);
const sharesToAtoms = (shares: bigint, sv: bigint): bigint => (shares * sv) >> 96n;

async function main(): Promise<void> {
  const url = process.env.YDELTA_RPC_URL ?? process.env.RPC_HTTP_URL ?? 'https://api.mainnet-beta.solana.com';
  const conn = new Connection(url, 'confirmed');

  const vault = await fetchVault(conn, USDC);
  if (!vault) {
    console.log('no USDC vault');
    return;
  }
  const vaultPk = globalVaultPda(USDC)[0];
  const h = vault.header;
  console.log(`vault ${vaultPk.toBase58()}  mint=USDC  profiles=${vault.riskProfiles.length}  paused=${h.isPaused}`);
  console.log(`lendingPool(bank)=${h.lendingPool.toBase58()}`);

  let sumAssets = 0n;
  let sumPrincipal = 0n;
  let sumDeployed = 0n;
  let sumIdle = 0n;
  let sumEncumbered = 0n;
  let sumCuratorFee = 0n;
  console.log('\n--- per profile ---');
  for (const { profile } of vault.riskProfiles) {
    const b = profileBalancesUi(profile, DEC);
    const rp = riskProfileUi(profile, DEC);
    sumAssets += b.totalAssetsAtoms;
    sumPrincipal += b.totalPrincipalAtoms;
    sumDeployed += b.deployedPrincipalAtoms;
    sumIdle += b.idleAtoms;
    sumEncumbered += b.encumberedInOrdersAtoms;
    sumCuratorFee += b.accumulatedCuratorFeeAtoms;
    console.log(
      `#${profile.profileId} curator=${profile.curator.toBase58().slice(0, 6)} ` +
        `assets=${ui(b.totalAssetsAtoms)} principal=${ui(b.totalPrincipalAtoms)} ` +
        `deployed=${ui(b.deployedPrincipalAtoms)} idle=${ui(b.idleAtoms)} ` +
        `encumberedInOrders=${ui(b.encumberedInOrdersAtoms)} ` +
        `shares=${profile.totalShares} curatorFee=${ui(b.accumulatedCuratorFeeAtoms)} ` +
        `netAPY=${(rp.averageNetLenderRateBps / 100).toFixed(2)}%`,
    );
  }
  console.log('\n--- bookkeeping totals (sum of profiles) ---');
  console.log(
    `assets=${ui(sumAssets)} principal=${ui(sumPrincipal)} deployed=${ui(sumDeployed)} ` +
      `idle=${ui(sumIdle)} encumberedInOrders=${ui(sumEncumbered)} curatorFee=${ui(sumCuratorFee)}`,
  );

  // Actual on-chain: the vault's marginfi integration account asset position
  // in the pinned bank + the SPL staging vault balance.
  const integ = globalVaultIntegrationAccountPda(vaultPk)[0];
  const staging = globalVaultStagingPda(vaultPk)[0];
  const [maInfo, bankInfo, stagingInfo] = await Promise.all([
    conn.getAccountInfo(integ),
    conn.getAccountInfo(h.lendingPool),
    conn.getAccountInfo(staging),
  ]);
  let mfiAtoms = 0n;
  if (maInfo && bankInfo) {
    const ma = decodeMarginfiAccount(maInfo.data);
    const bank = decodeBank(bankInfo.data);
    const bal = findActiveBalance(ma, h.lendingPool);
    mfiAtoms = bal ? sharesToAtoms(bal.assetSharesFp48, bank.assetShareValueFp48) : 0n;
  }
  let stagingAtoms = 0n;
  if (stagingInfo && stagingInfo.data.length >= 72) {
    stagingAtoms = stagingInfo.data.readBigUInt64LE(64);
  }
  console.log('\n--- actual on-chain holdings ---');
  console.log(`marginfi integration assets = ${ui(mfiAtoms)} USDC  (acct ${integ.toBase58().slice(0, 6)})`);
  console.log(`SPL staging vault balance   = ${ui(stagingAtoms)} USDC  (acct ${staging.toBase58().slice(0, 6)})`);
  const actual = mfiAtoms + stagingAtoms;
  console.log(`actual total                = ${ui(actual)} USDC`);

  console.log('\n--- reconciliation ---');
  // Idle principal physically sits in marginfi; deployed principal has been
  // drained to market loans. So actual marginfi ≈ idle (+ accrued not yet swept).
  const diffIdle = mfiAtoms - sumIdle;
  console.log(`marginfi assets - bookkeeping idle = ${ui(diffIdle)} USDC (expect ~0; +ve = accrued/yield, -ve = SHORTFALL)`);
  console.log(`bookkeeping assets - principal     = ${ui(sumAssets - sumPrincipal)} USDC (accrued curator/yield buffer)`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
