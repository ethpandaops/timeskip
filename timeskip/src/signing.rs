use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};

use alloy_primitives::U256;
use anyhow::{Context, Result, anyhow};
use beacon_chain::test_utils::{BeaconChainHarness, EphemeralHarnessType, RelativeSyncCommittee};
use bls::{AggregateSignature, Keypair, SecretKey};
use rayon::prelude::*;
use ssz_types::BitVector;
use types::{
    BeaconState, ChainSpec, Domain, Epoch, EthSpec, Hash256, PayloadAttestationData,
    PayloadAttestationMessage, SignedRoot, Slot, SyncCommitteeContribution,
};

use crate::cli::SigningMode;

/// BLS12-381 scalar field order.
const BLS_MODULUS_HEX: &str = "73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001";

pub type Harness<E> = BeaconChainHarness<EphemeralHarnessType<E>>;

/// One validator's attestation, as signed.
#[derive(Debug, Clone, Copy)]
pub struct Vote {
    pub validator_index: usize,
    pub source: Epoch,
    pub target: Epoch,
}

/// Signs every duty for a freshly imported block so the next block can include it.
pub trait DutySigner<E: EthSpec> {
    /// Attest to `block_root` with every member of the committees at `state.slot()`.
    fn attest(
        &mut self,
        harness: &Harness<E>,
        state: &BeaconState<E>,
        state_root: Hash256,
        block_root: Hash256,
    ) -> Result<Vec<Vote>>;

    /// Sign `block_root` with the whole sync committee; returns the number of participants.
    fn sign_sync(
        &mut self,
        harness: &Harness<E>,
        state: &BeaconState<E>,
        block_root: Hash256,
    ) -> Result<usize>;
}

pub fn new_signer<E: EthSpec>(
    mode: SigningMode,
    keypairs: &[Keypair],
) -> Result<Box<dyn DutySigner<E>>> {
    Ok(match mode {
        SigningMode::Summed => Box::new(SummedSigner::new(keypairs)?),
        SigningMode::Harness => Box::new(HarnessSigner {
            validators: (0..keypairs.len()).collect(),
        }),
    })
}

/// Votes "payload present" for the Gloas head with every PTC member, one message per distinct member.
///
/// The op pool adds a member's signature once per PTC seat it holds, so the next block carries one full aggregate.
pub fn sign_payload_attestations<E: EthSpec>(
    harness: &Harness<E>,
    state: &BeaconState<E>,
    block_root: Hash256,
) -> Result<usize> {
    let slot = state.slot();
    let spec = &harness.spec;
    let ptc = state
        .get_ptc(slot, spec)
        .map_err(|e| anyhow!("PTC at {slot}: {e:?}"))?;
    let members: BTreeSet<usize> = ptc.0.iter().copied().collect();
    let epoch = slot.epoch(E::slots_per_epoch());
    let domain = spec.get_domain(
        epoch,
        Domain::PTCAttester,
        &spec.fork_at_epoch(epoch),
        state.genesis_validators_root(),
    );
    let data = PayloadAttestationData {
        beacon_block_root: block_root,
        slot,
        payload_present: true,
        blob_data_available: true,
    };
    let signing_root = data.signing_root(domain);

    let keypairs = &harness.validator_keypairs;
    let messages = members
        .into_par_iter()
        .map(|validator_index| {
            let keypair = keypairs
                .get(validator_index)
                .with_context(|| format!("no key for PTC member {validator_index}"))?;
            Ok(PayloadAttestationMessage {
                validator_index: validator_index as u64,
                data: data.clone(),
                signature: keypair.sk.sign(signing_root),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let count = messages.len();
    for message in messages {
        harness
            .chain
            .op_pool
            .insert_payload_attestation(&message, &ptc)
            .map_err(|e| anyhow!("op pool payload attestation: {e:?}"))?;
    }
    Ok(count)
}

/// Sync messages for `slot` are verified against the committee of `slot + 1`.
fn verifying_sync_committee<E: EthSpec>(
    slot: Slot,
    spec: &ChainSpec,
) -> Result<(RelativeSyncCommittee, u64)> {
    let period = |slot: Slot| {
        slot.epoch(E::slots_per_epoch())
            .sync_committee_period(spec)
            .map_err(|e| anyhow!("sync committee period at {slot}: {e:?}"))
    };
    let next_period = period(slot + 1)?;
    let relative = if period(slot)? == next_period {
        RelativeSyncCommittee::Current
    } else {
        RelativeSyncCommittee::Next
    };
    Ok((relative, next_period))
}

/// Per-validator signing with gossip verification, as `BeaconChainHarness` does in tests.
struct HarnessSigner {
    validators: Vec<usize>,
}

impl<E: EthSpec> DutySigner<E> for HarnessSigner {
    fn attest(
        &mut self,
        harness: &Harness<E>,
        state: &BeaconState<E>,
        state_root: Hash256,
        block_root: Hash256,
    ) -> Result<Vec<Vote>> {
        let slot = state.slot();
        let attestations =
            harness.make_attestations(&self.validators, state, state_root, block_root.into(), slot);
        let committees = state
            .get_beacon_committees_at_slot(slot)
            .map_err(|e| anyhow!("committees at {slot}: {e:?}"))?;

        let mut votes = Vec::new();
        for (committee, (committee_attestations, _)) in committees.iter().zip(&attestations) {
            let Some((attestation, _)) = committee_attestations.first() else {
                continue;
            };
            let data = attestation.data();
            votes.extend(committee.committee.iter().map(|&validator_index| Vote {
                validator_index,
                source: data.source.epoch,
                target: data.target.epoch,
            }));
        }

        harness.process_attestations(attestations, state);
        Ok(votes)
    }

    fn sign_sync(
        &mut self,
        harness: &Harness<E>,
        state: &BeaconState<E>,
        block_root: Hash256,
    ) -> Result<usize> {
        let slot = state.slot();
        let (relative, _) = verifying_sync_committee::<E>(slot, &harness.spec)?;
        let contributions: Vec<_> = harness
            .make_sync_contributions(state, block_root, slot, relative)
            .into_iter()
            .filter(|(_, aggregate)| aggregate.is_some())
            .collect();
        let participants = contributions
            .iter()
            .map(|(messages, _)| messages.len())
            .sum();
        harness
            .process_sync_contributions(contributions)
            .map_err(|e| anyhow!("sync contributions at {slot}: {e:?}"))?;
        Ok(participants)
    }
}

/// Signs each committee's shared message once with the sum of its members' secret keys.
///
/// BLS signatures are linear in the secret key, so this equals the aggregate of the members'
/// individual signatures. Aggregates go straight into the op pool, skipping gossip verification;
/// block import verifies them.
struct SummedSigner {
    scalars: Vec<U256>,
    modulus: U256,
    sync_keys: HashMap<u64, Vec<SecretKey>>,
}

impl SummedSigner {
    fn new(keypairs: &[Keypair]) -> Result<Self> {
        let modulus = U256::from_str_radix(BLS_MODULUS_HEX, 16)?;
        let scalars = keypairs
            .iter()
            .map(|keypair| U256::from_be_slice(keypair.sk.serialize().as_bytes()))
            .collect();
        Ok(Self {
            scalars,
            modulus,
            sync_keys: HashMap::new(),
        })
    }

    /// Sum of the members' secret keys; repeated members count once per occurrence.
    fn summed_key(&self, members: impl IntoIterator<Item = usize>) -> Result<SecretKey> {
        let mut sum = U256::ZERO;
        for index in members {
            let scalar = self
                .scalars
                .get(index)
                .with_context(|| format!("no key for validator {index}"))?;
            sum = sum.add_mod(*scalar, self.modulus);
        }
        SecretKey::deserialize(&sum.to_be_bytes::<32>())
            .map_err(|e| anyhow!("summed key is not a valid secret key: {e:?}"))
    }

    fn sync_subcommittee_keys<E: EthSpec>(
        &mut self,
        harness: &Harness<E>,
        state: &BeaconState<E>,
        relative: RelativeSyncCommittee,
        period: u64,
    ) -> Result<&[SecretKey]> {
        if !self.sync_keys.contains_key(&period) {
            let committee = match relative {
                RelativeSyncCommittee::Current => state.current_sync_committee(),
                RelativeSyncCommittee::Next => state.next_sync_committee(),
            }
            .map_err(|e| anyhow!("sync committee: {e:?}"))?
            .clone();

            let keys = committee
                .pubkeys
                .as_ref()
                .chunks(E::sync_subcommittee_size())
                .map(|subcommittee| {
                    let members = subcommittee
                        .iter()
                        .map(|pubkey| {
                            harness
                                .chain
                                .validator_index(pubkey)
                                .map_err(|e| anyhow!("validator index: {e:?}"))?
                                .with_context(|| {
                                    format!("unknown sync committee member {pubkey:?}")
                                })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    self.summed_key(members)
                })
                .collect::<Result<Vec<_>>>()?;
            self.sync_keys.insert(period, keys);
        }

        self.sync_keys
            .get(&period)
            .map(Vec::as_slice)
            .context("sync keys missing after insert")
    }
}

impl<E: EthSpec> DutySigner<E> for SummedSigner {
    fn attest(
        &mut self,
        harness: &Harness<E>,
        state: &BeaconState<E>,
        state_root: Hash256,
        block_root: Hash256,
    ) -> Result<Vec<Vote>> {
        let slot = state.slot();
        let spec = &harness.spec;
        let fork = spec.fork_at_epoch(slot.epoch(E::slots_per_epoch()));
        let committees = state
            .get_beacon_committees_at_slot(slot)
            .map_err(|e| anyhow!("committees at {slot}: {e:?}"))?;

        let mut votes = Vec::new();
        for committee in committees {
            let mut attestation = harness
                .produce_unaggregated_attestation_for_block(
                    slot,
                    committee.index,
                    block_root,
                    Cow::Borrowed(state),
                    state_root,
                    None,
                )
                .map_err(|e| anyhow!("attestation data at {slot}: {e:?}"))?;
            for position in 0..committee.committee.len() {
                attestation
                    .set_aggregation_bit(position, true)
                    .map_err(|e| anyhow!("aggregation bit: {e:?}"))?;
            }

            let data = attestation.data().clone();
            let domain = spec.get_domain(
                data.target.epoch,
                Domain::BeaconAttester,
                &fork,
                state.genesis_validators_root(),
            );
            let key = self.summed_key(committee.committee.iter().copied())?;
            let mut signature = AggregateSignature::infinity();
            signature.add_assign(&key.sign(data.signing_root(domain)));
            *attestation.signature_mut() = signature;

            let mut attesting_indices: Vec<u64> = committee
                .committee
                .iter()
                .map(|&index| index as u64)
                .collect();
            attesting_indices.sort_unstable();
            harness
                .chain
                .op_pool
                .insert_attestation(attestation, attesting_indices)
                .map_err(|e| anyhow!("op pool attestation: {e:?}"))?;

            votes.extend(committee.committee.iter().map(|&validator_index| Vote {
                validator_index,
                source: data.source.epoch,
                target: data.target.epoch,
            }));
        }

        Ok(votes)
    }

    fn sign_sync(
        &mut self,
        harness: &Harness<E>,
        state: &BeaconState<E>,
        block_root: Hash256,
    ) -> Result<usize> {
        let slot = state.slot();
        let spec = harness.spec.clone();
        let (relative, period) = verifying_sync_committee::<E>(slot, &spec)?;
        let epoch = slot.epoch(E::slots_per_epoch());
        let domain = spec.get_domain(
            epoch,
            Domain::SyncCommittee,
            &spec.fork_at_epoch(epoch),
            state.genesis_validators_root(),
        );
        let message = block_root.signing_root(domain);

        let mut participants = 0;
        let keys = self.sync_subcommittee_keys(harness, state, relative, period)?;
        let signatures: Vec<_> = keys.par_iter().map(|key| key.sign(message)).collect();
        for (subcommittee_index, subcommittee_signature) in signatures.iter().enumerate() {
            let mut aggregation_bits = BitVector::new();
            for position in 0..E::sync_subcommittee_size() {
                aggregation_bits
                    .set(position, true)
                    .map_err(|e| anyhow!("sync aggregation bit: {e:?}"))?;
            }
            let mut signature = AggregateSignature::infinity();
            signature.add_assign(subcommittee_signature);

            harness
                .chain
                .op_pool
                .insert_sync_contribution(SyncCommitteeContribution {
                    slot,
                    beacon_block_root: block_root,
                    subcommittee_index: subcommittee_index as u64,
                    aggregation_bits,
                    signature,
                })
                .map_err(|e| anyhow!("op pool sync contribution: {e:?}"))?;
            participants += E::sync_subcommittee_size();
        }

        Ok(participants)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::test_utils::generate_deterministic_keypairs;

    #[test]
    fn summed_key_signature_equals_aggregate_of_individual_signatures() {
        let keypairs = generate_deterministic_keypairs(8);
        let signer = SummedSigner::new(&keypairs).expect("signer");
        let members = [0, 3, 3, 5, 7];
        let message = Hash256::repeat_byte(7);

        let mut individual = AggregateSignature::infinity();
        for &index in &members {
            individual.add_assign(&keypairs[index].sk.sign(message));
        }
        let mut summed = AggregateSignature::infinity();
        summed.add_assign(&signer.summed_key(members).expect("key").sign(message));

        assert_eq!(individual, summed);
    }
}
