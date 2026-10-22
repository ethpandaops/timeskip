use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use types::{Address, Epoch, Slot};

/// Build a finalized chain for every past slot of a devnet whose genesis lies in the past.
#[derive(Parser, Debug)]
#[command(name = "timeskip", version)]
pub struct Cli {
    /// Directory holding `config.yaml` and `genesis.ssz` (ethereum-package `el_cl_genesis_data`).
    #[arg(long, value_name = "DIR")]
    pub testnet_dir: PathBuf,

    /// Mnemonic the genesis validators were derived from.
    #[arg(long, env = "TIMESKIP_MNEMONIC", hide_env_values = true)]
    pub mnemonic: String,

    /// Number of validators derived from the mnemonic; must equal the genesis validator count.
    #[arg(long)]
    pub validator_count: usize,

    /// Engine API endpoint of the execution client that builds and executes the payloads.
    #[arg(long, value_name = "URL")]
    pub engine_url: String,

    /// JWT secret shared with the execution client.
    #[arg(long, value_name = "FILE")]
    pub jwt_secret: PathBuf,

    /// Stop after the block at this slot. Without a stop flag, run up to the wall-clock slot.
    #[arg(long, value_name = "SLOT", conflicts_with = "until_finalized_epoch")]
    pub until_slot: Option<u64>,

    /// Stop once the head state's finalized epoch reaches this value.
    #[arg(long, value_name = "EPOCH")]
    pub until_finalized_epoch: Option<u64>,

    /// Directory for blocks, envelopes, manifest, summary and the slashing-protection interchange.
    #[arg(long, value_name = "DIR")]
    pub output_dir: PathBuf,

    /// How attestations and sync committee contributions are signed and pooled.
    #[arg(long, value_enum, default_value_t = SigningMode::Summed)]
    pub signing: SigningMode,

    /// Fee recipient for every payload.
    #[arg(long, default_value = "0x8943545177806ED17B9F23F0a21ee5948eCaa776")]
    pub fee_recipient: Address,

    /// Graffiti for every block, at most 32 bytes.
    #[arg(long, default_value = "timeskip")]
    pub graffiti: String,

    /// Skip writing a JSON copy of each block next to its SSZ.
    #[arg(long)]
    pub no_json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SigningMode {
    /// One signature per committee with the summed secret keys, inserted straight into the op pool.
    Summed,
    /// One signature per validator, passed through gossip verification like the test harness.
    Harness,
}

impl SigningMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summed => "summed",
            Self::Harness => "harness",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum StopCondition {
    WallClock,
    Slot(Slot),
    FinalizedEpoch(Epoch),
}

impl Cli {
    pub fn stop_condition(&self) -> StopCondition {
        match (self.until_slot, self.until_finalized_epoch) {
            (Some(slot), _) => StopCondition::Slot(Slot::new(slot)),
            (None, Some(epoch)) => StopCondition::FinalizedEpoch(Epoch::new(epoch)),
            (None, None) => StopCondition::WallClock,
        }
    }
}
