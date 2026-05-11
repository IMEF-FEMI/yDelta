import { PublicKey } from '@solana/web3.js';
import BigNumber from 'bignumber.js';
import { Vault } from '../vault';
import { MarginfiReader } from './client';
import { getBankRates, aprToApy } from './bank';

export type VaultProfileApy = {
  profileId: number;
  /** Supply APY from the underlying marginfi bank (idle vault deposits earn this). */
  supplyApy: number;
  /** Yield "delta" — the spread yDelta lenders extract from borrowers on top of
   *  marginfi's base. Derived from `total_weighted_rate_bps / deployed_principal_atoms`. */
  deltaApy: number;
  /** Sum of supply + delta — what depositors see on a profile. */
  totalApy: number;
  /** Fraction of profile capital deployed in loans vs idle in marginfi. */
  deployedFraction: number;
  /** Assets-side idle (`total_assets - deployed - encumbered`). This is the
   *  amount currently earning marginfi supply yield, *including* prior
   *  accrued yield. Distinct from `Vault.idleAtoms` (principal-based gate
   *  for new-order sizing). */
  idleAtoms: BigNumber;
  /** Atoms currently working in loans. */
  deployedAtoms: BigNumber;
};

/** Compute a per-profile APY breakdown (supply yield + yDelta delta yield). */
export async function getVaultProfileApy({
  reader,
  vault,
  profileId,
  debtBank,
}: {
  reader: MarginfiReader;
  vault: Vault;
  profileId: number;
  debtBank: PublicKey;
}): Promise<VaultProfileApy> {
  const profile = vault.getRiskProfile(profileId);
  if (!profile) throw new Error(`profile ${profileId} not found in vault ${vault.address.toBase58()}`);

  const totalAssets = new BigNumber(profile.totalAssetsAtoms.toString());
  const deployed = new BigNumber(profile.deployedPrincipalAtoms.toString());
  const encumbered = new BigNumber(profile.encumberedInOrdersAtoms.toString());

  const deployedFraction = totalAssets.isZero() ? 0 : deployed.div(totalAssets).toNumber();
  const idleFraction = 1 - deployedFraction;
  const idleAtoms = BigNumber.max(totalAssets.minus(deployed).minus(encumbered), new BigNumber(0));

  // Supply APY: idle atoms earn marginfi's lending rate. Weighted by idle share.
  const bankRates = await getBankRates(reader, debtBank);
  const supplyApy = bankRates.supplyApy * idleFraction;

  // Delta APY: weighted-average lender rate on deployed atoms.
  let deltaApy = 0;
  if (!deployed.isZero()) {
    // total_weighted_rate_bps = Σ(principal × lender_rate_bps); divide for avg.
    const avgRateBps = new BigNumber(profile.totalWeightedRateBps.toString())
      .div(deployed)
      .toNumber();
    deltaApy = aprToApy(avgRateBps / 10_000) * deployedFraction;
  }

  return {
    profileId,
    supplyApy,
    deltaApy,
    totalApy: supplyApy + deltaApy,
    deployedFraction,
    idleAtoms,
    deployedAtoms: deployed,
  };
}
