use anyhow::{Result, anyhow, ensure};
use bls::{Keypair, SecretKey};
use eth2_wallet::bip39::{Language, Mnemonic, Seed};
use eth2_wallet::{KeyType, recover_validator_secret_from_mnemonic};
use rayon::prelude::*;
use types::{BeaconState, EthSpec};

/// Derives voting keypairs `0..count` on the EIP-2334 path used by `lcli mnemonic-validators`.
pub fn derive_keypairs(phrase: &str, count: usize) -> Result<Vec<Keypair>> {
    let mnemonic = Mnemonic::from_phrase(phrase.trim(), Language::English)
        .map_err(|e| anyhow!("invalid mnemonic: {e:?}"))?;
    let seed = Seed::new(&mnemonic, "");

    (0..count)
        .into_par_iter()
        .map(|index| {
            let index = u32::try_from(index)?;
            let (secret, _path) =
                recover_validator_secret_from_mnemonic(seed.as_bytes(), index, KeyType::Voting)
                    .map_err(|e| anyhow!("deriving key {index}: {e:?}"))?;
            let sk = SecretKey::deserialize(secret.as_bytes())
                .map_err(|e| anyhow!("decoding key {index}: {e:?}"))?;
            Ok(Keypair::from_components(sk.public_key(), sk))
        })
        .collect()
}

/// Fails unless `keypairs[i]` is the key of genesis validator `i` for every validator.
pub fn verify_against_genesis<E: EthSpec>(
    keypairs: &[Keypair],
    state: &BeaconState<E>,
) -> Result<()> {
    let validators = state.validators();
    ensure!(
        validators.len() == keypairs.len(),
        "genesis has {} validators but {} keys were derived",
        validators.len(),
        keypairs.len()
    );

    for (index, (validator, keypair)) in validators.iter().zip(keypairs).enumerate() {
        let derived = keypair.pk.compress();
        ensure!(
            validator.pubkey == derived,
            "validator {index}: derived pubkey {derived:?} does not match genesis {:?}",
            validator.pubkey
        );
    }

    Ok(())
}
