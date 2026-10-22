<p align="center">
  <img src="assets/logo.svg" width="112" alt="timeskip logo">
</p>

<h1 align="center">timeskip</h1>

<p align="center">Start Ethereum devnets already finalized.</p>

---

A fresh devnet with 12 second slots needs about 26 minutes to reach its first finalized checkpoint. timeskip generates that early history offline instead. Give it a genesis whose genesis time is in the past, and in a few seconds it builds a valid, finalized chain covering every slot since genesis. Every slot gets a block with full attestations and a full sync aggregate, and under Gloas a self-built payload envelope. With [ethereum-package](https://github.com/ethpandaops/ethereum-package), every beacon node is at the generated, finalized head about 40 to 90 seconds after `kurtosis run`.

## How it works

```
 config.yaml + genesis.ssz          execution client (reth)
 mnemonic                                  ^
      |                                    | Engine API: build and execute payloads
      v                                    |
 +-----------------------------------------+-------------+
 | timeskip (Lighthouse BeaconChainHarness)              |
 |  for each slot since genesis:                         |
 |    sign the head's duties into the op pool            |
 |    produce the block (and Gloas envelope)             |
 |    full import, require it to become the head         |
 +-------------------------------------------------------+
      |
      v
 blocks, envelopes, payload requests, manifest, summary,
 slashing-protection interchange
```

1. Loads `config.yaml` and `genesis.ssz`, derives the validator keys from the mnemonic on the EIP-2334 path, and checks that they match the genesis validators.
2. Before each block, signs the current head's duties and puts them straight into the op pool:
   - one attestation per committee, signed once with the sum of the members' secret keys. BLS signatures are linear in the secret key, so this equals the aggregate of every member's signature;
   - a full sync committee contribution for each subcommittee;
   - under Gloas, a payload-present vote from every PTC member.
3. Produces the block on Lighthouse's `BeaconChainHarness`. The execution client builds the payload over the Engine API. Under Gloas, the proposer self-builds a zero-value bid and signs the envelope.
4. Imports the block, and its envelope under Gloas, through Lighthouse's full import path. That covers every signature, the state transition, `engine_newPayload` against the execution client, and fork choice. The run fails unless the block becomes the head; under Gloas it also requires the execution client to return `VALID` for the envelope.
5. Writes each block out as it goes, then the run files: manifest, summary, the per-key slashing-protection records and the final forkchoice update.

Generation runs at roughly 80 to 100 slots per second on Fulu and 35 to 45 on Gloas, after about 2 seconds of startup.

## Quick start with ethereum-package

> The ethereum-package integration is still landing and is not in a release yet.

Add a `fast_forward` section to your package config:

```yaml
participants:
  - el_type: geth
    cl_type: lighthouse
    supernode: true
  - el_type: geth
    cl_type: teku
fast_forward:
  enabled: true
  until: finalized
  history: checkpoint
  image: ghcr.io/ethpandaops/timeskip:latest
  el_image: ghcr.io/paradigmxyz/reth:latest
```

```sh
kurtosis run github.com/ethpandaops/ethereum-package --args-file config.yaml
```

The package places genesis in the past, runs timeskip inside the enclave against a private reth, inserts the chain into every client, and starts the validator clients with the slashing-protection records once insertion is done.

| Key | Default | Description |
|---|---|---|
| `enabled` | `false` | Generate the early history instead of starting from genesis. |
| `until` | last scheduled fork, `finalized` | How far to generate: a state (`activated`, `justified`, `finalized`), a fork name, or a map `{fork: <name>, state: <state>}` or `{epoch: <n>, state: <state>}`. Forks scheduled after the generated history activate live. |
| `history` | `checkpoint` | `checkpoint`: beacon nodes checkpoint-sync from the last finalized checkpoint of the generated chain. `full`: every node imports every generated block. |
| `image` | | The timeskip image. |
| `el_image` | `ghcr.io/paradigmxyz/reth:latest` | The execution client that builds the payloads during generation. |

## CLI

```sh
export TIMESKIP_MNEMONIC="<mnemonic the genesis validators were derived from>"
timeskip \
  --testnet-dir ./network-configs \
  --validator-count 128 \
  --engine-url http://127.0.0.1:8551 \
  --jwt-secret ./jwt.hex \
  --until-finalized-epoch 2 \
  --output-dir ./out
```

The execution client must be a fresh, peerless node started from the same genesis, with the same JWT secret. Gloas needs an execution client that speaks `engine_forkchoiceUpdatedV4`, `engine_getPayloadV6` and `engine_newPayloadV5`; reth 2.8.0 does.

| Flag | Default | Description |
|---|---|---|
| `--testnet-dir <DIR>` | required | Directory holding `config.yaml` and `genesis.ssz`, such as ethereum-package's `el_cl_genesis_data`. |
| `--mnemonic <MNEMONIC>` | required | Mnemonic of the genesis validators. Also read from `TIMESKIP_MNEMONIC`. |
| `--validator-count <N>` | required | Number of keys to derive. Must equal the genesis validator count. |
| `--engine-url <URL>` | required | Engine API endpoint of the execution client. |
| `--jwt-secret <FILE>` | required | JWT secret shared with the execution client. |
| `--output-dir <DIR>` | required | Where the output is written. |
| `--until-slot <SLOT>` | | Stop after the block at this slot. |
| `--until-finalized-epoch <EPOCH>` | | Stop once the head's finalized epoch reaches this value. |
| `--signing <MODE>` | `summed` | `summed` signs once per committee with the summed keys. `harness` signs once per validator and runs each message through gossip verification; it is about 14 times slower and gave identical chains in testing. |
| `--fee-recipient <ADDRESS>` | `0x8943545177806ED17B9F23F0a21ee5948eCaa776` | Fee recipient for every payload. |
| `--graffiti <TEXT>` | `timeskip` | Graffiti for every block, at most 32 bytes. |
| `--no-json` | | Skip the JSON copy of each block and envelope. |

Without `--until-slot` or `--until-finalized-epoch`, timeskip runs up to the current wall-clock slot. Logging follows `RUST_LOG`.

## Output

```
out/
  blocks/<slot>.ssz        signed beacon block, plus <slot>.json unless --no-json
  envelopes/<slot>.ssz     signed execution payload envelope (Gloas slots), plus <slot>.json
  payloads/<slot>.json     engine_newPayload request for the slot's payload (V4, or V5 for Gloas)
  manifest.json            per block: slot, root, parent, fork, EL block hash and number,
                           proposer, attestation and sync counts, sizes, parent payload status
  summary.json             genesis, head, finality and fork-choice checkpoints,
                           EL head, safe and finalized hashes, watermarks, timings
  interchange.json         EIP-3076 slashing-protection interchange, one record per key
  attesters.json           validators that attested at each slot, with source and target epochs
  forkchoice.json          final engine_forkchoiceUpdated request (V3, or V4 for a Gloas head)
```

To insert the chain, post the blocks, and their envelopes under Gloas, to the beacon nodes in slot order. `payloads/` and `forkchoice.json` can bring an execution client to the generated head directly over the Engine API. Import `interchange.json` into the validator clients before they start, so they never sign anything that conflicts with the generated history.

## Supported forks

| Fork | Status |
|---|---|
| Fulu | Supported. Tested with Lighthouse, Prysm, Teku, Nimbus, Lodestar and Grandine. |
| Gloas | Supported, from genesis or a later epoch. Tested with the same six clients. |
| Electra | Payload export is implemented but untested. |
| Earlier forks | Not supported. |

Both the `mainnet` and `minimal` presets are handled; testing so far used `mainnet`.

## Docker images

Images are published to [`ghcr.io/ethpandaops/timeskip`](https://github.com/ethpandaops/timeskip/pkgs/container/timeskip) for `linux/amd64` and `linux/arm64`. The entrypoint is `timeskip`. The image also ships `python3` for ethereum-package's insertion scripts.

| Tag | Built from |
|---|---|
| `latest` | The latest `master` commit, against Lighthouse `unstable`. |
| `<sha7>` | Each `master` commit, against Lighthouse `unstable`. |
| `<ref>-<sha7>` | Manual builds against another Lighthouse branch or tag. |
| `<version>` | GitHub releases. |

## Building locally

timeskip is a crate in Lighthouse's workspace, not a standalone Cargo project. It uses Lighthouse's internal crates, and those need the `[patch.crates-io]` overrides in Lighthouse's workspace manifest. `scripts/build.sh` clones Lighthouse, copies the `timeskip/` crate into it, adds it to the workspace members and builds it. The build needs the same tools as Lighthouse: a stable Rust toolchain, `cmake` and `clang`.

```sh
./scripts/build.sh                                  # sigp/lighthouse@unstable, cloned into ./lighthouse
./scripts/build.sh --ref <branch|tag|commit-sha>    # another Lighthouse ref
./scripts/build.sh --lighthouse-dir ../lighthouse   # use an existing checkout, which it modifies
./scripts/build.sh --prepare-only                   # overlay the crate without building
```

The binary ends up in `lighthouse/target/release/timeskip`. Run `cargo fmt`, `cargo clippy -p timeskip` and `cargo test -p timeskip` from `lighthouse/`, and edit the crate here in `timeskip/`, since every build copies it again.

To build the image:

```sh
docker build --build-arg LIGHTHOUSE_REF=unstable -t timeskip .
```

CI builds against Lighthouse `unstable` on pull requests and pushes to `master`, and again daily to catch upstream API changes.

## Limitations

- Generated blocks carry no transactions or blobs.
- Payloads are only checked by the execution client that built them. Clients can disagree: reth 2.8.0 accepts Gloas payloads when the genesis lacks the EIP-8282 builder deposit contracts, and geth rejects them.
- The final head's attestations and sync messages are left unsigned, so those validators can still sign them live.
- A Gloas genesis has its slot 1 block build on an empty parent payload. The spec doesn't define this yet, but all six clients accept it.

## Known client quirks

These came up in end-to-end runs. ethereum-package works around the first two.

- **Teku** doesn't start gossip after blocks arrive over the beacon API. Restart its beacon node after insertion.
- **Prysm** can send forkchoice updates out of order after envelopes. Wait for its execution client's head after each envelope.
- **Grandine** 3.0.0 keeps its execution client's head one payload behind under Gloas. It also routes `/eth/v4/validator/blocks` as GET only, so validator clients that POST there can't propose Gloas blocks through it.
- **Crossing a fork inside the generated history:** Lighthouse and Grandine re-broadcast pre-fork blocks received over the API on the post-fork gossip topic, and Prysm bans them for it. Stop the history before the fork with `until`, and let the fork activate live.

## License

[GPL-3.0](LICENSE). timeskip builds on [Lighthouse](https://github.com/sigp/lighthouse), which is licensed under Apache-2.0.
