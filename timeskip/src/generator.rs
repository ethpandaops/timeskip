use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use beacon_chain::graffiti_calculator::GraffitiSettings;
use beacon_chain::payload_envelope_verification::EnvelopeSource;
use beacon_chain::test_utils::{BeaconChainHarness, EphemeralHarnessType};
use beacon_chain::{
    AvailabilityProcessingStatus, BeaconBlockResponseWrapper, ChainConfig, NotifyExecutionLayer,
    ProduceBlockVerification,
};
use bls::{Keypair, PublicKeyBytes, Signature};
use eth2::types::{BuilderConfig, GraffitiPolicy};
use execution_layer::{BlockByNumberQuery, ExecutionLayer};
use proto_array::{ExecutionVerdict, PayloadBlockHash, PayloadStatus};
use sensitive_url::SensitiveUrl;
use serde::Serialize;
use slot_clock::{SlotClock, SystemTimeSlotClock};
use state_processing::state_advance::complete_state_advance;
use task_executor::TaskExecutor;
use tracing::{debug, info, warn};
use types::consts::gloas::BUILDER_INDEX_SELF_BUILD;
use types::execution::BlockProductionVersion;
use types::{
    Address, BeaconState, BlockImportSource, ChainSpec, Checkpoint, Domain, Epoch, EthSpec,
    ExecPayload, ExecutionBlockHash, Graffiti, Hash256, SignedBeaconBlock,
    SignedExecutionPayloadEnvelope, SignedRoot, Slot,
};

use crate::cli::{SigningMode, StopCondition};
use crate::engine_export;
use crate::output::{OutputWriter, ParentPayload};
use crate::signing::{DutySigner, Harness, new_signer, sign_payload_attestations};
use crate::timing::PhaseTimings;
use crate::watermark::Watermarks;

const EL_HEAD_TIMEOUT: Duration = Duration::from_secs(15);
const EL_HEAD_POLL: Duration = Duration::from_millis(50);

pub struct GeneratorConfig {
    pub engine_url: String,
    pub jwt_secret: PathBuf,
    pub fee_recipient: Address,
    pub graffiti: Graffiti,
    pub signing: SigningMode,
    pub output_dir: PathBuf,
    pub write_json: bool,
}

/// Head, checkpoints and EL tip once generation stops.
#[derive(Debug, Clone, Serialize)]
pub struct ChainTip {
    pub head_slot: u64,
    pub head_root: Hash256,
    pub head_state_root: Hash256,
    pub finality_checkpoints: FinalityCheckpoints,
    pub fork_choice: ForkChoiceCheckpoints,
    pub el_head: ElHead,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinalityCheckpoints {
    pub previous_justified: Checkpoint,
    pub current_justified: Checkpoint,
    pub finalized: Checkpoint,
}

#[derive(Debug, Clone, Serialize)]
pub struct ForkChoiceCheckpoints {
    pub justified: Checkpoint,
    pub finalized: Checkpoint,
}

#[derive(Debug, Clone, Serialize)]
pub struct ElHead {
    pub number: u64,
    pub hash: ExecutionBlockHash,
    pub safe_hash: ExecutionBlockHash,
    pub finalized_hash: ExecutionBlockHash,
    /// Whether the execution client's `latest` block matched `hash` after the final update.
    pub confirmed_by_engine: bool,
}

/// A block imported as the new head, with its envelope from Gloas on.
struct Proposal<E: EthSpec> {
    block: Arc<SignedBeaconBlock<E>>,
    block_root: Hash256,
    post_state: BeaconState<E>,
    envelope: Option<SignedExecutionPayloadEnvelope<E>>,
    parent_payload: Option<ParentPayload>,
}

/// Drives a `BeaconChainHarness` backed by a real execution client over past slots.
pub struct Generator<E: EthSpec> {
    harness: Harness<E>,
    signer: Box<dyn DutySigner<E>>,
    output: OutputWriter,
    watermarks: Watermarks,
    timings: PhaseTimings,
    graffiti: Graffiti,
    state: BeaconState<E>,
    state_root: Hash256,
    _executor_exit: async_channel::Sender<()>,
}

impl<E: EthSpec> Generator<E> {
    pub fn new(
        spec: ChainSpec,
        genesis_state: BeaconState<E>,
        keypairs: Vec<Keypair>,
        config: GeneratorConfig,
    ) -> Result<Self> {
        let (executor_exit, exit) = async_channel::bounded(1);
        let (shutdown_tx, _) = futures::channel::mpsc::channel(1);
        let executor = TaskExecutor::new(tokio::runtime::Handle::current(), exit, shutdown_tx);

        let engine_url = SensitiveUrl::parse(&config.engine_url)
            .map_err(|e| anyhow!("invalid engine url: {e:?}"))?;
        let execution_layer = ExecutionLayer::<E>::from_config(
            execution_layer::Config {
                execution_endpoint: Some(engine_url),
                secret_file: Some(config.jwt_secret.clone()),
                suggested_fee_recipient: Some(config.fee_recipient),
                default_datadir: config.output_dir.clone(),
                ..Default::default()
            },
            executor,
        )
        .map_err(|e| anyhow!("building execution layer: {e:?}"))?;

        let signer = new_signer::<E>(config.signing, &keypairs)?;
        let validator_count = keypairs.len();
        let mut state = genesis_state.clone();
        let state_root = state
            .canonical_root()
            .map_err(|e| anyhow!("genesis state root: {e:?}"))?;

        let harness = BeaconChainHarness::<EphemeralHarnessType<E>>::builder(E::default())
            .spec(Arc::new(spec))
            .keypairs(keypairs)
            .genesis_state_ephemeral_store(genesis_state)
            .execution_layer(Some(execution_layer))
            .chain_config(ChainConfig::default())
            .build();

        Ok(Self {
            harness,
            signer,
            output: OutputWriter::create(&config.output_dir, config.write_json)?,
            watermarks: Watermarks::new(validator_count),
            timings: PhaseTimings::default(),
            graffiti: config.graffiti,
            state,
            state_root,
            _executor_exit: executor_exit,
        })
    }

    pub fn spec(&self) -> &ChainSpec {
        &self.harness.spec
    }

    pub fn timings(&self) -> &PhaseTimings {
        &self.timings
    }

    pub fn output(&self) -> &OutputWriter {
        &self.output
    }

    pub fn watermarks(&self) -> &Watermarks {
        &self.watermarks
    }

    pub fn genesis_validators_root(&self) -> Hash256 {
        self.harness.chain.genesis_validators_root
    }

    pub fn genesis_block_root(&self) -> Hash256 {
        self.harness.chain.genesis_block_root
    }

    pub fn genesis_time(&self) -> u64 {
        self.harness.chain.genesis_time
    }

    pub fn pubkeys(&self) -> Vec<PublicKeyBytes> {
        self.harness
            .validator_keypairs
            .iter()
            .map(|keypair| keypair.pk.compress())
            .collect()
    }

    pub fn wall_clock_slot(&self) -> Result<Slot> {
        let clock = SystemTimeSlotClock::new(
            Slot::new(0),
            Duration::from_secs(self.genesis_time()),
            self.spec().get_slot_duration(),
        );
        clock.now().context("genesis is in the future")
    }

    fn head_finalized_epoch(&self) -> Epoch {
        self.harness
            .chain
            .head_snapshot()
            .beacon_state
            .finalized_checkpoint()
            .epoch
    }

    /// Produces and imports a block for every slot until `stop` is met.
    pub async fn run(&mut self, stop: StopCondition) -> Result<()> {
        let slots_per_epoch = E::slots_per_epoch();
        let started = Instant::now();
        let mut slot = self.state.slot() + 1;

        loop {
            let done = match stop {
                StopCondition::WallClock => slot > self.wall_clock_slot()?,
                StopCondition::Slot(last) => slot > last,
                StopCondition::FinalizedEpoch(epoch) => {
                    let limit = (epoch + 4).start_slot(slots_per_epoch);
                    ensure!(
                        slot <= limit,
                        "finalized epoch {epoch} not reached by slot {limit}; head finalized epoch is {}",
                        self.head_finalized_epoch()
                    );
                    self.head_finalized_epoch() >= epoch
                }
            };
            if done {
                break;
            }

            let slot_started = Instant::now();
            self.step(slot).await?;
            self.timings.record("slot_total", slot_started.elapsed());

            if slot % slots_per_epoch == 0 {
                let head_state = &self.harness.chain.head_snapshot().beacon_state;
                info!(
                    %slot,
                    head = ?self.harness.head_block_root(),
                    justified = %head_state.current_justified_checkpoint().epoch,
                    finalized = %head_state.finalized_checkpoint().epoch,
                    elapsed_ms = started.elapsed().as_millis(),
                    "Epoch boundary"
                );
            }
            slot += 1;
        }

        Ok(())
    }

    /// Attests to and sync-signs the head just before the next block, so the final head's duties stay unsigned.
    fn sign_head_duties(&mut self) -> Result<()> {
        let head_root = self.harness.head_block_root();
        self.state
            .build_caches(&self.harness.spec)
            .map_err(|e| anyhow!("building caches at slot {}: {e:?}", self.state.slot()))?;

        let started = Instant::now();
        let votes = self
            .signer
            .attest(&self.harness, &self.state, self.state_root, head_root)?;
        self.timings.record("attest", started.elapsed());
        self.watermarks
            .record_slot_attestations(self.state.slot(), &votes);

        if self.state.fork_name_unchecked().gloas_enabled() {
            let started = Instant::now();
            sign_payload_attestations(&self.harness, &self.state, head_root)?;
            self.timings.record("ptc", started.elapsed());
        }

        let started = Instant::now();
        let sync_participants = self
            .signer
            .sign_sync(&self.harness, &self.state, head_root)?;
        self.timings.record("sync", started.elapsed());
        if sync_participants == 0 {
            warn!(slot = %self.state.slot(), "No sync committee contributions for this block");
        }
        Ok(())
    }

    async fn step(&mut self, slot: Slot) -> Result<()> {
        let spec = self.harness.spec.clone();
        self.sign_head_duties()?;
        self.harness.set_current_slot(slot);

        let started = Instant::now();
        let mut pre_state = self.state.clone();
        complete_state_advance(&mut pre_state, Some(self.state_root), slot, None, &spec)
            .map_err(|e| anyhow!("advancing to slot {slot}: {e:?}"))?;
        pre_state
            .build_caches(&spec)
            .map_err(|e| anyhow!("building caches at slot {slot}: {e:?}"))?;
        let proposer_index = pre_state
            .get_beacon_proposer_index(slot, &spec)
            .map_err(|e| anyhow!("proposer at slot {slot}: {e:?}"))?;
        self.timings.record("advance", started.elapsed());

        let started = Instant::now();
        let randao_reveal = self
            .harness
            .sign_randao_reveal(&pre_state, proposer_index, slot);
        let randao_signing = started.elapsed();

        let proposal = if spec.fork_name_at_slot::<E>(slot).gloas_enabled() {
            self.propose_gloas(
                pre_state,
                slot,
                proposer_index,
                randao_reveal,
                randao_signing,
            )
            .await?
        } else {
            self.propose(
                pre_state,
                slot,
                proposer_index,
                randao_reveal,
                randao_signing,
            )
            .await?
        };
        self.watermarks.record_block(proposer_index, slot);

        let started = Instant::now();
        let block = &proposal.block;
        let parent_sync_participants = block
            .message()
            .body()
            .sync_aggregate()
            .map(|aggregate| aggregate.num_set_bits())
            .unwrap_or_default();
        self.output.write_block(
            block,
            proposal.block_root,
            parent_sync_participants,
            proposal.envelope.as_ref(),
            proposal.parent_payload,
        )?;
        self.timings.record("write", started.elapsed());

        self.state_root = block.state_root();
        self.state = proposal.post_state;
        Ok(())
    }

    /// Produces, signs and imports a pre-Gloas block, whose payload is embedded.
    async fn propose(
        &mut self,
        pre_state: BeaconState<E>,
        slot: Slot,
        proposer_index: usize,
        randao_reveal: Signature,
        mut signing: Duration,
    ) -> Result<Proposal<E>> {
        let spec = self.harness.spec.clone();
        let started = Instant::now();
        let response = self
            .harness
            .chain
            .produce_block_on_state(
                pre_state,
                None,
                slot,
                randao_reveal,
                self.graffiti_settings(),
                ProduceBlockVerification::VerifyRandao,
                None,
                BlockProductionVersion::FullV2,
            )
            .await
            .map_err(|e| anyhow!("producing block at slot {slot}: {e:?}"))?;
        let BeaconBlockResponseWrapper::Full(response) = response else {
            bail!("expected a full block at slot {slot}");
        };
        self.timings.record("produce", started.elapsed());

        let started = Instant::now();
        let proposer = self.keypair(proposer_index)?;
        let block = Arc::new(response.block.sign(
            &proposer.sk,
            &response.state.fork(),
            response.state.genesis_validators_root(),
            &spec,
        ));
        signing += started.elapsed();
        self.timings.record("sign_block", signing);

        let block_root = block.canonical_root();
        let started = Instant::now();
        self.harness
            .process_block(slot, block_root, (block.clone(), response.blob_items))
            .await
            .map_err(|e| anyhow!("importing block at slot {slot}: {e:?}"))?;
        self.timings.record("import", started.elapsed());
        ensure!(
            self.harness.head_block_root() == block_root,
            "block {block_root:?} at slot {slot} imported but head is {:?}",
            self.harness.head_block_root()
        );

        Ok(Proposal {
            block,
            block_root,
            post_state: response.state,
            envelope: None,
            parent_payload: None,
        })
    }

    /// Self-builds a Gloas block and envelope, imports both via gossip, and requires a FULL head.
    async fn propose_gloas(
        &mut self,
        pre_state: BeaconState<E>,
        slot: Slot,
        proposer_index: usize,
        randao_reveal: Signature,
        mut signing: Duration,
    ) -> Result<Proposal<E>> {
        let spec = self.harness.spec.clone();
        let chain = self.harness.chain.clone();

        let parent_root = *pre_state
            .get_block_root(slot - 1)
            .map_err(|e| anyhow!("parent root at slot {slot}: {e:?}"))?;
        let (head_root, parent_payload_status, parent_envelope) = {
            let head = chain.canonical_head.cached_head();
            (
                head.head_block_root(),
                head.head_payload_status(),
                head.snapshot.execution_envelope.clone(),
            )
        };
        ensure!(
            head_root == parent_root,
            "slot {slot} builds on {parent_root:?} but the head is {head_root:?}"
        );
        let latest_bid = pre_state
            .latest_execution_payload_bid()
            .map_err(|e| anyhow!("latest bid at slot {slot}: {e:?}"))?;
        let parent_payload_hash = latest_bid.block_hash();
        let parent_is_genesis = parent_payload_hash == ExecutionBlockHash::zero();

        let started = Instant::now();
        let (block, post_state, _, _, payload_contents, _) =
            Box::pin(chain.produce_block_on_state_gloas(
                pre_state,
                None,
                parent_root,
                parent_payload_status,
                parent_envelope,
                slot,
                randao_reveal,
                self.graffiti_settings(),
                ProduceBlockVerification::VerifyRandao,
                BuilderConfig::empty(),
            ))
            .await
            .map_err(|e| anyhow!("producing Gloas block at slot {slot}: {e:?}"))?;
        self.timings.record("produce", started.elapsed());

        let (envelope, _, blobs) =
            payload_contents.with_context(|| format!("no self-built envelope at slot {slot}"))?;
        ensure!(
            blobs.is_empty(),
            "unexpected blobs in the payload at slot {slot}"
        );
        let bid = block
            .body()
            .signed_execution_payload_bid()
            .map_err(|e| anyhow!("block at slot {slot} has no bid: {e:?}"))?
            .message();
        ensure!(
            bid.builder_index() == BUILDER_INDEX_SELF_BUILD && bid.value() == 0,
            "slot {slot}: expected a zero-value self-build bid, got builder {} value {}",
            bid.builder_index(),
            bid.value()
        );
        let parent_payload = if bid.parent_block_hash() == parent_payload_hash {
            ParentPayload::Full
        } else {
            ParentPayload::Empty
        };
        ensure!(
            parent_payload == ParentPayload::Full || parent_is_genesis,
            "slot {slot} builds on an EMPTY parent {parent_root:?} (parent status \
             {parent_payload_status:?}); every non-genesis parent must be FULL"
        );

        let started = Instant::now();
        let proposer = self.keypair(proposer_index)?;
        let fork = post_state.fork();
        let genesis_validators_root = post_state.genesis_validators_root();
        let block = Arc::new(block.sign(&proposer.sk, &fork, genesis_validators_root, &spec));
        signing += started.elapsed();

        let started = Instant::now();
        let domain = spec.get_domain(
            slot.epoch(E::slots_per_epoch()),
            Domain::BeaconBuilder,
            &fork,
            genesis_validators_root,
        );
        let envelope = SignedExecutionPayloadEnvelope {
            signature: proposer.sk.sign(envelope.signing_root(domain)),
            message: Arc::unwrap_or_clone(envelope),
        };
        let envelope_signing = started.elapsed();
        self.timings.record("sign_block", signing);
        self.timings.record("sign_envelope", envelope_signing);

        let block_root = block.canonical_root();
        chain.pending_payload_envelopes.write().remove(block_root);
        ensure!(
            envelope.message.beacon_block_root == block_root,
            "envelope at slot {slot} commits to {:?}, block root is {block_root:?}",
            envelope.message.beacon_block_root
        );

        let started = Instant::now();
        self.import_gloas_block(slot, block_root, block.clone())
            .await?;
        self.timings.record("import", started.elapsed());

        let started = Instant::now();
        self.import_envelope(slot, block_root, &envelope).await?;
        self.timings.record("envelope", started.elapsed());

        let head = chain.canonical_head.cached_head();
        ensure!(
            head.head_block_root() == block_root
                && head.head_payload_status() == PayloadStatus::Full,
            "slot {slot}: head is {:?} ({:?}), expected FULL {block_root:?}",
            head.head_block_root(),
            head.head_payload_status()
        );
        debug!(%slot, ?parent_payload, block_hash = ?envelope.message.payload.block_hash, "Gloas slot done");

        Ok(Proposal {
            block,
            block_root,
            post_state,
            envelope: Some(envelope),
            parent_payload: Some(parent_payload),
        })
    }

    async fn import_gloas_block(
        &self,
        slot: Slot,
        block_root: Hash256,
        block: Arc<SignedBeaconBlock<E>>,
    ) -> Result<()> {
        let chain = &self.harness.chain;
        let verified = chain.verify_block_for_gossip(block).await.map_err(|e| {
            anyhow!(
                "block at slot {slot} failed gossip verification: {:?}",
                e.into_block_error()
            )
        })?;
        let status = chain
            .process_block(
                block_root,
                verified,
                NotifyExecutionLayer::Yes,
                BlockImportSource::Gossip,
                || Ok(()),
            )
            .await
            .map_err(|e| anyhow!("importing block at slot {slot}: {e:?}"))?;
        ensure!(
            matches!(status, AvailabilityProcessingStatus::Imported(..)),
            "block at slot {slot} not imported: {status:?}"
        );
        Ok(())
    }

    /// Imports an envelope as gossip would and requires a VALID verdict from the EL.
    async fn import_envelope(
        &self,
        slot: Slot,
        block_root: Hash256,
        envelope: &SignedExecutionPayloadEnvelope<E>,
    ) -> Result<()> {
        let chain = &self.harness.chain;
        let verified = chain
            .verify_envelope_for_gossip(Arc::new(envelope.clone()), EnvelopeSource::Gossip)
            .await
            .map_err(|e| anyhow!("envelope at slot {slot} failed gossip verification: {e:?}"))?;
        let status = chain
            .process_execution_payload_envelope(
                block_root,
                verified,
                NotifyExecutionLayer::Yes,
                BlockImportSource::Gossip,
                || Ok(()),
            )
            .await
            .map_err(|e| anyhow!("importing envelope at slot {slot}: {e:?}"))?;
        ensure!(
            matches!(status, AvailabilityProcessingStatus::Imported(..)),
            "envelope at slot {slot} not imported: {status:?}"
        );
        let verdict = chain
            .canonical_head
            .fork_choice_read_lock()
            .get_block_execution_status_assuming_full(&block_root)
            .map_err(|e| anyhow!("execution status at slot {slot}: {e:?}"))?;
        ensure!(
            matches!(verdict, Some(ExecutionVerdict::Valid)),
            "envelope payload at slot {slot} is {verdict:?}, the EL must return VALID"
        );
        chain.recompute_head_at_current_slot().await;
        Ok(())
    }

    fn keypair(&self, validator_index: usize) -> Result<&Keypair> {
        self.harness
            .validator_keypairs
            .get(validator_index)
            .with_context(|| format!("no key for validator {validator_index}"))
    }

    fn graffiti_settings(&self) -> GraffitiSettings {
        GraffitiSettings::new(
            Some(self.graffiti),
            Some(GraffitiPolicy::PreserveUserGraffiti),
        )
    }

    /// Re-sends the final forkchoice update and waits for the EL to report the head payload.
    pub async fn finish(&self) -> Result<ChainTip> {
        self.harness.chain.recompute_head_at_current_slot().await;
        let head = self.harness.chain.head_snapshot();
        let head_state = &head.beacon_state;
        let gloas_head = head.beacon_block.fork_name_unchecked().gloas_enabled();
        let (hash, number) = if gloas_head {
            let envelope = head
                .execution_envelope
                .as_ref()
                .context("Gloas head has no imported envelope")?;
            (
                envelope.payload_header.block_hash,
                envelope.payload_header.block_number,
            )
        } else {
            let payload = head
                .beacon_block
                .message()
                .body()
                .execution_payload()
                .map_err(|e| anyhow!("head block has no payload: {e:?}"))?;
            (payload.block_hash(), payload.block_number())
        };
        let safe_hash = self.payload_hash_at(head_state.current_justified_checkpoint().root)?;
        let finalized_hash = self.payload_hash_at(head_state.finalized_checkpoint().root)?;
        self.output.write_json_file(
            "forkchoice.json",
            &engine_export::forkchoice_request(hash, safe_hash, finalized_hash, gloas_head),
        )?;

        let confirmed_by_engine = self.wait_for_el_head(hash).await;
        if !confirmed_by_engine {
            warn!(
                ?hash,
                "Execution client did not report the head payload as latest"
            );
        }

        Ok(ChainTip {
            head_slot: head.beacon_block.slot().as_u64(),
            head_root: head.beacon_block_root,
            head_state_root: head.beacon_state_root(),
            finality_checkpoints: FinalityCheckpoints {
                previous_justified: head_state.previous_justified_checkpoint(),
                current_justified: head_state.current_justified_checkpoint(),
                finalized: head_state.finalized_checkpoint(),
            },
            fork_choice: ForkChoiceCheckpoints {
                justified: self.harness.justified_checkpoint(),
                finalized: self.harness.finalized_checkpoint(),
            },
            el_head: ElHead {
                number,
                hash,
                safe_hash,
                finalized_hash,
                confirmed_by_engine,
            },
        })
    }

    /// Checkpoint EL hash for forkchoice updates (the bid's parent from Gloas on); zero is genesis.
    fn payload_hash_at(&self, root: Hash256) -> Result<ExecutionBlockHash> {
        let root = if root.is_zero() {
            self.genesis_block_root()
        } else {
            root
        };
        let proto_block = self
            .harness
            .chain
            .canonical_head
            .fork_choice_read_lock()
            .get_block(&root)
            .with_context(|| format!("block {root:?} is not in fork choice"))?;
        if proto_block.execution_payload_parent_hash.is_some() {
            return match proto_block.checkpoint_payload_block_hash() {
                PayloadBlockHash::Hash(hash) => Ok(hash),
                PayloadBlockHash::PreMerge => bail!("checkpoint {root:?} is pre-merge"),
            };
        }
        let block = self
            .harness
            .chain
            .get_blinded_block(&root)
            .map_err(|e| anyhow!("reading block {root:?}: {e:?}"))?
            .with_context(|| format!("block {root:?} is not in the store"))?;
        let payload = block
            .message()
            .body()
            .execution_payload()
            .map_err(|e| anyhow!("block {root:?} has no payload: {e:?}"))?;
        Ok(payload.block_hash())
    }

    async fn wait_for_el_head(&self, expected: ExecutionBlockHash) -> bool {
        let Some(execution_layer) = self.harness.chain.execution_layer.as_ref() else {
            return false;
        };
        let deadline = Instant::now() + EL_HEAD_TIMEOUT;
        while Instant::now() < deadline {
            match execution_layer
                .get_block_by_number(BlockByNumberQuery::Tag("latest"))
                .await
            {
                Ok(Some(block)) if block.block_hash == expected => return true,
                Ok(_) => {}
                Err(e) => warn!(error = ?e, "Querying the EL head failed"),
            }
            tokio::time::sleep(EL_HEAD_POLL).await;
        }
        false
    }
}
