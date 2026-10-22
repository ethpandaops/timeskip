use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use ssz::Encode;
use types::{
    EthSpec, ExecPayload, ExecutionBlockHash, ForkName, Hash256, SignedBeaconBlock,
    SignedExecutionPayloadEnvelope,
};

use crate::engine_export;

/// Whether a Gloas block's bid builds on its parent's payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ParentPayload {
    Full,
    Empty,
}

/// One generated block as listed in `manifest.json`.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestEntry {
    pub slot: u64,
    pub root: Hash256,
    pub parent_root: Hash256,
    pub version: ForkName,
    pub el_block_hash: ExecutionBlockHash,
    pub el_block_number: u64,
    pub proposer_index: u64,
    pub attestations: usize,
    pub sync_participants: usize,
    pub ssz_bytes: usize,
    /// Size of `envelopes/<slot>.ssz`; `None` before Gloas.
    pub envelope_ssz_bytes: Option<usize>,
    /// `None` before Gloas.
    pub parent_payload: Option<ParentPayload>,
}

/// Writes `blocks/<slot>.ssz` (and `.json`) as blocks are produced, then the run files.
pub struct OutputWriter {
    dir: PathBuf,
    write_json: bool,
    manifest: Vec<ManifestEntry>,
}

impl OutputWriter {
    pub fn create(dir: &Path, write_json: bool) -> Result<Self> {
        for sub in ["blocks", "payloads", "envelopes"] {
            let path = dir.join(sub);
            fs::create_dir_all(&path).with_context(|| format!("creating {}", path.display()))?;
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            write_json,
            manifest: Vec::new(),
        })
    }

    fn write_object<T: Encode + Serialize>(
        &self,
        sub: &str,
        slot: u64,
        version: ForkName,
        object: &T,
    ) -> Result<usize> {
        let bytes = object.as_ssz_bytes();
        let ssz_path = self.dir.join(sub).join(format!("{slot}.ssz"));
        fs::write(&ssz_path, &bytes).with_context(|| format!("writing {}", ssz_path.display()))?;

        if self.write_json {
            let json_path = self.dir.join(sub).join(format!("{slot}.json"));
            let document = serde_json::json!({ "version": version, "data": object });
            fs::write(&json_path, serde_json::to_vec(&document)?)
                .with_context(|| format!("writing {}", json_path.display()))?;
        }
        Ok(bytes.len())
    }

    /// Writes a block and its Gloas envelope; the EL hash and newPayload follow the payload.
    pub fn write_block<E: EthSpec>(
        &mut self,
        block: &SignedBeaconBlock<E>,
        root: Hash256,
        sync_participants: usize,
        envelope: Option<&SignedExecutionPayloadEnvelope<E>>,
        parent_payload: Option<ParentPayload>,
    ) -> Result<()> {
        let slot = block.slot().as_u64();
        let version = block.fork_name_unchecked();
        let ssz_bytes = self.write_object("blocks", slot, version, block)?;
        let message = block.message();

        let (request, el_block_hash, el_block_number, envelope_ssz_bytes) = match envelope {
            Some(envelope) => {
                let bytes = self.write_object("envelopes", slot, version, envelope)?;
                let payload = &envelope.message.payload;
                (
                    engine_export::envelope_new_payload_request(block, envelope)?,
                    payload.block_hash,
                    payload.block_number,
                    Some(bytes),
                )
            }
            None => {
                let payload = message
                    .execution_payload()
                    .map_err(|e| anyhow!("block {slot} has no execution payload: {e:?}"))?;
                (
                    engine_export::new_payload_request(block)?,
                    payload.block_hash(),
                    payload.block_number(),
                    None,
                )
            }
        };
        let payload_path = self.dir.join("payloads").join(format!("{slot}.json"));
        fs::write(&payload_path, serde_json::to_vec(&request)?)
            .with_context(|| format!("writing {}", payload_path.display()))?;

        self.manifest.push(ManifestEntry {
            slot,
            root,
            parent_root: message.parent_root(),
            version,
            el_block_hash,
            el_block_number,
            proposer_index: message.proposer_index(),
            attestations: message.body().attestations_len(),
            sync_participants,
            ssz_bytes,
            envelope_ssz_bytes,
            parent_payload,
        });
        Ok(())
    }

    pub fn manifest(&self) -> &[ManifestEntry] {
        &self.manifest
    }

    pub fn write_json_file<T: Serialize + ?Sized>(&self, name: &str, value: &T) -> Result<PathBuf> {
        let path = self.dir.join(name);
        fs::write(&path, serde_json::to_vec_pretty(value)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    pub fn write_manifest(&self) -> Result<PathBuf> {
        self.write_json_file("manifest.json", &self.manifest)
    }
}
