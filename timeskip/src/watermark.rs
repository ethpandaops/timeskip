use bls::PublicKeyBytes;
use serde::Serialize;
use types::{Epoch, Hash256, Slot};

use crate::signing::Vote;

const INTERCHANGE_FORMAT_VERSION: &str = "5";

/// The latest block and attestation each validator signed while generating the chain.
pub struct Watermarks {
    blocks: Vec<Option<Slot>>,
    attestations: Vec<Option<AttestationMark>>,
    slot_attesters: Vec<SlotAttesters>,
}

/// Every validator that attested at one slot, so the watermark can be rebuilt for any handover slot.
#[derive(Debug, Serialize)]
pub struct SlotAttesters {
    slot: Slot,
    source: Epoch,
    target: Epoch,
    validators: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
struct AttestationMark {
    source: Epoch,
    target: Epoch,
}

/// EIP-3076 slashing-protection interchange.
#[derive(Debug, Serialize)]
pub struct Interchange {
    metadata: InterchangeMetadata,
    data: Vec<InterchangeRecord>,
}

#[derive(Debug, Serialize)]
struct InterchangeMetadata {
    interchange_format_version: &'static str,
    genesis_validators_root: Hash256,
}

#[derive(Debug, Serialize)]
struct InterchangeRecord {
    pubkey: PublicKeyBytes,
    signed_blocks: Vec<SignedBlockRecord>,
    signed_attestations: Vec<SignedAttestationRecord>,
}

#[derive(Debug, Serialize)]
struct SignedBlockRecord {
    slot: Slot,
}

#[derive(Debug, Serialize)]
struct SignedAttestationRecord {
    source_epoch: Epoch,
    target_epoch: Epoch,
}

/// Highest values across all keys, for the handover gate.
#[derive(Debug, Clone, Serialize)]
pub struct WatermarkSummary {
    pub keys: usize,
    pub keys_with_block: usize,
    pub keys_with_attestation: usize,
    pub max_block_slot: Option<Slot>,
    pub max_target_epoch: Option<Epoch>,
    pub max_source_epoch: Option<Epoch>,
    pub min_target_epoch: Option<Epoch>,
}

impl Watermarks {
    pub fn new(validator_count: usize) -> Self {
        Self {
            blocks: vec![None; validator_count],
            attestations: vec![None; validator_count],
            slot_attesters: Vec::new(),
        }
    }

    /// Records one slot's attestations, which are only published inside the next block.
    pub fn record_slot_attestations(&mut self, slot: Slot, votes: &[Vote]) {
        for vote in votes {
            self.record_attestation(vote.validator_index, vote.source, vote.target);
        }
        let Some(first) = votes.first() else {
            return;
        };
        self.slot_attesters.push(SlotAttesters {
            slot,
            source: first.source,
            target: first.target,
            validators: votes.iter().map(|vote| vote.validator_index).collect(),
        });
    }

    pub fn slot_attesters(&self) -> &[SlotAttesters] {
        &self.slot_attesters
    }

    pub fn record_block(&mut self, validator_index: usize, slot: Slot) {
        if let Some(mark) = self.blocks.get_mut(validator_index) {
            *mark = Some(mark.map_or(slot, |prev| prev.max(slot)));
        }
    }

    fn record_attestation(&mut self, validator_index: usize, source: Epoch, target: Epoch) {
        if let Some(mark) = self.attestations.get_mut(validator_index) {
            let newer = mark.is_none_or(|prev| target > prev.target);
            if newer {
                *mark = Some(AttestationMark { source, target });
            }
        }
    }

    /// One record per key: its last signed block slot and its last attestation source and target.
    pub fn to_interchange(
        &self,
        pubkeys: &[PublicKeyBytes],
        genesis_validators_root: Hash256,
    ) -> Interchange {
        let data = pubkeys
            .iter()
            .enumerate()
            .map(|(index, pubkey)| InterchangeRecord {
                pubkey: *pubkey,
                signed_blocks: self
                    .blocks
                    .get(index)
                    .copied()
                    .flatten()
                    .map(|slot| SignedBlockRecord { slot })
                    .into_iter()
                    .collect(),
                signed_attestations: self
                    .attestations
                    .get(index)
                    .copied()
                    .flatten()
                    .map(|mark| SignedAttestationRecord {
                        source_epoch: mark.source,
                        target_epoch: mark.target,
                    })
                    .into_iter()
                    .collect(),
            })
            .collect();

        Interchange {
            metadata: InterchangeMetadata {
                interchange_format_version: INTERCHANGE_FORMAT_VERSION,
                genesis_validators_root,
            },
            data,
        }
    }

    pub fn summary(&self) -> WatermarkSummary {
        let blocks = self.blocks.iter().flatten();
        let attestations = self.attestations.iter().flatten();
        WatermarkSummary {
            keys: self.blocks.len(),
            keys_with_block: blocks.clone().count(),
            keys_with_attestation: attestations.clone().count(),
            max_block_slot: blocks.max().copied(),
            max_target_epoch: attestations.clone().map(|m| m.target).max(),
            max_source_epoch: attestations.clone().map(|m| m.source).max(),
            min_target_epoch: attestations.map(|m| m.target).min(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interchange_has_one_record_per_key_with_latest_marks() {
        let pubkeys = vec![PublicKeyBytes::empty(); 3];
        let mut watermarks = Watermarks::new(3);
        watermarks.record_block(0, Slot::new(5));
        watermarks.record_block(0, Slot::new(3));
        watermarks.record_attestation(0, Epoch::new(1), Epoch::new(2));
        watermarks.record_attestation(0, Epoch::new(0), Epoch::new(1));
        watermarks.record_attestation(1, Epoch::new(2), Epoch::new(3));

        let interchange = watermarks.to_interchange(&pubkeys, Hash256::ZERO);
        let json = serde_json::to_value(&interchange).expect("serializes");

        assert_eq!(json["metadata"]["interchange_format_version"], "5");
        assert_eq!(json["data"].as_array().map(Vec::len), Some(3));
        assert_eq!(
            json["data"][0]["signed_blocks"],
            serde_json::json!([{ "slot": "5" }])
        );
        assert_eq!(
            json["data"][0]["signed_attestations"],
            serde_json::json!([{ "source_epoch": "1", "target_epoch": "2" }])
        );
        assert_eq!(json["data"][1]["signed_blocks"], serde_json::json!([]));
        assert_eq!(
            json["data"][2]["signed_attestations"],
            serde_json::json!([])
        );
    }

    #[test]
    fn slot_attestations_are_kept_per_slot_and_raise_each_key_mark() {
        let vote = |validator_index, source, target| Vote {
            validator_index,
            source: Epoch::new(source),
            target: Epoch::new(target),
        };
        let mut watermarks = Watermarks::new(4);
        watermarks.record_slot_attestations(Slot::new(31), &[vote(0, 0, 0), vote(2, 0, 0)]);
        watermarks.record_slot_attestations(Slot::new(32), &[vote(1, 0, 1)]);
        watermarks.record_slot_attestations(Slot::new(33), &[]);

        let json = serde_json::to_value(watermarks.slot_attesters()).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!([
                { "slot": "31", "source": "0", "target": "0", "validators": [0, 2] },
                { "slot": "32", "source": "0", "target": "1", "validators": [1] },
            ])
        );
        let summary = watermarks.summary();
        assert_eq!(summary.keys_with_attestation, 3);
        assert_eq!(summary.max_target_epoch, Some(Epoch::new(1)));
    }
}
