mod cli;
mod engine_export;
mod generator;
mod keys;
mod network;
mod output;
mod signing;
mod timing;
mod watermark;

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Result, bail, ensure};
use clap::Parser;
use serde::Serialize;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use types::{EthSpec, GRAFFITI_BYTES_LEN, Graffiti, Hash256, MainnetEthSpec, MinimalEthSpec};

use crate::cli::Cli;
use crate::generator::{ChainTip, Generator, GeneratorConfig};
use crate::timing::{EngineCalls, PhaseSummary};
use crate::watermark::WatermarkSummary;

#[derive(Debug, Serialize)]
struct Summary {
    genesis: GenesisInfo,
    genesis_block_root: Hash256,
    validator_count: usize,
    signing: &'static str,
    first_slot: Option<u64>,
    last_slot: Option<u64>,
    blocks: usize,
    envelopes: usize,
    first_gloas_slot: Option<u64>,
    missed_slots: Vec<u64>,
    #[serde(flatten)]
    tip: ChainTip,
    wall_clock_slot_at_end: Option<u64>,
    watermarks: WatermarkSummary,
    timings: Timings,
}

#[derive(Debug, Serialize)]
struct GenesisInfo {
    genesis_time: String,
    genesis_validators_root: Hash256,
}

#[derive(Debug, Serialize)]
struct Timings {
    startup_ms: BTreeMap<&'static str, f64>,
    generation_ms: f64,
    finish_ms: f64,
    total_ms: f64,
    slots_per_second: f64,
    phases: BTreeMap<String, PhaseSummary>,
    engine_calls: EngineCalls,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("timeskip=info,warn")),
        )
        .with_target(false)
        .init();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    match network::preset(&cli.testnet_dir)?.as_str() {
        "mainnet" => generate::<MainnetEthSpec>(cli).await,
        "minimal" => generate::<MinimalEthSpec>(cli).await,
        other => bail!("unsupported preset {other}"),
    }
}

fn parse_graffiti(value: &str) -> Result<Graffiti> {
    let bytes = value.as_bytes();
    ensure!(
        bytes.len() <= GRAFFITI_BYTES_LEN,
        "graffiti is longer than {GRAFFITI_BYTES_LEN} bytes"
    );
    let mut graffiti = [0u8; GRAFFITI_BYTES_LEN];
    graffiti[..bytes.len()].copy_from_slice(bytes);
    Ok(Graffiti::from(graffiti))
}

fn millis(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1e3
}

async fn generate<E: EthSpec>(cli: Cli) -> Result<()> {
    let run_started = Instant::now();
    let mut startup_ms = BTreeMap::new();

    let started = Instant::now();
    let network = network::load::<E>(&cli.testnet_dir)?;
    startup_ms.insert("load_network", millis(started));

    let started = Instant::now();
    let keypairs = keys::derive_keypairs(&cli.mnemonic, cli.validator_count)?;
    keys::verify_against_genesis(&keypairs, &network.genesis_state)?;
    startup_ms.insert("derive_keys", millis(started));
    info!(
        validators = keypairs.len(),
        "Derived keys match the genesis validators"
    );

    let started = Instant::now();
    let mut generator = Generator::new(
        network.spec,
        network.genesis_state,
        keypairs,
        GeneratorConfig {
            engine_url: cli.engine_url.clone(),
            jwt_secret: cli.jwt_secret.clone(),
            fee_recipient: cli.fee_recipient,
            graffiti: parse_graffiti(&cli.graffiti)?,
            signing: cli.signing,
            output_dir: cli.output_dir.clone(),
            write_json: !cli.no_json,
        },
    )?;
    startup_ms.insert("build_harness", millis(started));
    info!(
        genesis_time = generator.genesis_time(),
        genesis_block_root = ?generator.genesis_block_root(),
        signing = cli.signing.as_str(),
        "Harness ready"
    );

    let engine_before = EngineCalls::snapshot();
    let started = Instant::now();
    generator.run(cli.stop_condition()).await?;
    let generation_ms = millis(started);

    let started = Instant::now();
    let tip = generator.finish().await?;
    let finish_ms = millis(started);
    let engine_calls = EngineCalls::snapshot().since(&engine_before);

    let manifest = generator.output().manifest();
    let first_slot = manifest.first().map(|entry| entry.slot);
    let last_slot = manifest.last().map(|entry| entry.slot);
    let produced: std::collections::HashSet<u64> =
        manifest.iter().map(|entry| entry.slot).collect();
    let missed_slots = match last_slot {
        Some(last) => (1..=last).filter(|slot| !produced.contains(slot)).collect(),
        None => vec![],
    };

    let interchange = generator
        .watermarks()
        .to_interchange(&generator.pubkeys(), generator.genesis_validators_root());
    let summary = Summary {
        genesis: GenesisInfo {
            genesis_time: generator.genesis_time().to_string(),
            genesis_validators_root: generator.genesis_validators_root(),
        },
        genesis_block_root: generator.genesis_block_root(),
        validator_count: cli.validator_count,
        signing: cli.signing.as_str(),
        first_slot,
        last_slot,
        blocks: manifest.len(),
        envelopes: manifest
            .iter()
            .filter(|entry| entry.envelope_ssz_bytes.is_some())
            .count(),
        first_gloas_slot: manifest
            .iter()
            .find(|entry| entry.version.gloas_enabled())
            .map(|entry| entry.slot),
        missed_slots,
        tip,
        wall_clock_slot_at_end: generator.wall_clock_slot().ok().map(|slot| slot.as_u64()),
        watermarks: generator.watermarks().summary(),
        timings: Timings {
            startup_ms,
            generation_ms,
            finish_ms,
            total_ms: millis(run_started),
            slots_per_second: manifest.len() as f64 / (generation_ms / 1e3),
            phases: generator.timings().summary(),
            engine_calls,
        },
    };

    let output = generator.output();
    output.write_manifest()?;
    output.write_json_file("interchange.json", &interchange)?;
    output.write_json_file("attesters.json", generator.watermarks().slot_attesters())?;
    output.write_json_file("summary.json", &summary)?;
    print_report(&summary);
    Ok(())
}

fn print_report(summary: &Summary) {
    let tip = &summary.tip;
    let checkpoints = &tip.finality_checkpoints;
    let timings = &summary.timings;
    println!(
        "timeskip: {} blocks, {} envelopes (slots {:?}..{:?}, Gloas from {:?}) in {:.0} ms, {:.1} slots/s, signing={}",
        summary.blocks,
        summary.envelopes,
        summary.first_slot,
        summary.last_slot,
        summary.first_gloas_slot,
        timings.generation_ms,
        timings.slots_per_second,
        summary.signing
    );
    println!(
        "head slot {} root {:?}; justified {} finalized {} ({:?}); EL #{} {:?} confirmed={}",
        tip.head_slot,
        tip.head_root,
        checkpoints.current_justified.epoch,
        checkpoints.finalized.epoch,
        checkpoints.finalized.root,
        tip.el_head.number,
        tip.el_head.hash,
        tip.el_head.confirmed_by_engine
    );
    println!(
        "{:<12} {:>6} {:>10} {:>8} {:>8} {:>8} {:>8}",
        "phase", "count", "total_ms", "mean", "p50", "p90", "max"
    );
    for (phase, s) in &timings.phases {
        println!(
            "{:<12} {:>6} {:>10.1} {:>8.2} {:>8.2} {:>8.2} {:>8.2}",
            phase, s.count, s.total_ms, s.mean_ms, s.p50_ms, s.p90_ms, s.max_ms
        );
    }
    println!(
        "{:<24} {:>6} {:>10} {:>8}",
        "engine call", "count", "total_ms", "mean"
    );
    for (method, s) in &timings.engine_calls.methods {
        println!(
            "{:<24} {:>6} {:>10.1} {:>8.2}",
            method, s.count, s.total_ms, s.mean_ms
        );
    }
    println!(
        "startup {:?}; finish {:.0} ms; total {:.0} ms",
        timings.startup_ms, timings.finish_ms, timings.total_ms
    );
}
