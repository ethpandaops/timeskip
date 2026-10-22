use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use eth2_network_config::Eth2NetworkConfig;
use types::{BeaconState, ChainSpec, EthSpec};

/// Chain spec and genesis state of a devnet.
pub struct Network<E: EthSpec> {
    pub spec: ChainSpec,
    pub genesis_state: BeaconState<E>,
}

/// Returns the `PRESET_BASE` of the testnet dir.
pub fn preset(dir: &Path) -> Result<String> {
    let config = Eth2NetworkConfig::load(dir.to_path_buf())
        .map_err(|e| anyhow!("loading {}: {e}", dir.display()))?;
    Ok(config.config.preset_base)
}

pub fn load<E: EthSpec>(dir: &Path) -> Result<Network<E>> {
    let config = Eth2NetworkConfig::load(dir.to_path_buf())
        .map_err(|e| anyhow!("loading {}: {e}", dir.display()))?;
    let spec = config.chain_spec::<E>().map_err(|e| anyhow!(e))?;

    let path = dir.join("genesis.ssz");
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let genesis_state = BeaconState::from_ssz_bytes(&bytes, &spec)
        .map_err(|e| anyhow!("decoding {}: {e:?}", path.display()))?;
    ensure!(
        genesis_state.slot() == 0,
        "genesis.ssz is at slot {}, expected 0",
        genesis_state.slot()
    );

    Ok(Network {
        spec,
        genesis_state,
    })
}
