<p align="center">
  <img src="assets/logo.svg" width="112" alt="timeskip logo">
</p>

<h1 align="center">timeskip</h1>

<p align="center">Start Ethereum devnets already finalized.</p>

---

A new devnet takes about 26 minutes to finalize. timeskip generates that history instead: give it a genesis with a `genesis_time` in the past and it produces a valid, finalized chain for every slot since, in seconds. With [ethereum-package](https://github.com/ethpandaops/ethereum-package) every node is at the finalized head 40 to 90 seconds after `kurtosis run`.

## How it works

- Runs Lighthouse's `BeaconChainHarness` on a manual slot clock, with a real reth building and executing payloads over the Engine API.
- For every slot since genesis: sign the duties, produce the block (plus the signed payload envelope under Gloas), import it through Lighthouse's full verification path, including `engine_newPayload`.
- Each committee, sync and PTC message is signed once with the sum of the members' secret keys, which is the aggregate signature. That is why it runs at 80 to 100 slots per second (35 to 45 on Gloas).
- Writes out the blocks, envelopes, payload requests, a finalized checkpoint anchor and EIP-3076 slashing-protection records, so real clients can load the chain and validators can take over without equivocating.

## Quick start

```yaml
participants:
  - el_type: geth
    cl_type: lighthouse
    supernode: true
  - el_type: geth
    cl_type: teku
fast_forward:
  enabled: true
```

```sh
kurtosis run github.com/ethpandaops/ethereum-package --args-file config.yaml
```

The package puts genesis in the past, runs timeskip in the enclave against a private reth, loads the chain into every client, and starts the validator clients at the handover slot with matching slashing-protection records.

| Key | Default | |
|---|---|---|
| `enabled` | `false` | |
| `until` | `finalized` at the last scheduled fork | `activated`, `justified`, `finalized`, a fork name, or `{fork\|epoch, state}` |
| `history` | `checkpoint` | `checkpoint`: nodes sync from the generated finalized checkpoint. `full`: every node imports every block |
| `image` | `ghcr.io/ethpandaops/timeskip:latest` | |
| `el_image` | `ghcr.io/paradigmxyz/reth:latest` | builds the payloads during generation |

The integration is not in an ethereum-package release yet.

## CLI

```sh
export TIMESKIP_MNEMONIC="<genesis validator mnemonic>"
timeskip \
  --testnet-dir ./network-configs \
  --validator-count 128 \
  --engine-url http://127.0.0.1:8551 \
  --jwt-secret ./jwt.hex \
  --until-finalized-epoch 2 \
  --output-dir ./out
```

The execution client must be fresh, peerless, started from the same genesis, with the same JWT. Gloas needs `engine_forkchoiceUpdatedV4`, `engine_getPayloadV6` and `engine_newPayloadV5` (reth 2.8.0 works).

| Flag | |
|---|---|
| `--testnet-dir` | directory with `config.yaml` and `genesis.ssz` |
| `--mnemonic` | or `TIMESKIP_MNEMONIC` |
| `--validator-count` | must equal the genesis validator count |
| `--engine-url`, `--jwt-secret` | execution client |
| `--output-dir` | |
| `--until-slot`, `--until-finalized-epoch` | stop condition; default is the current wall-clock slot |
| `--ahead-slots` | run this far past the wall clock, for blocks to be published at their slot times |
| `--max-slot` | hard cap, e.g. the slot before a fork that should activate live |
| `--signing` | `summed` (default) or `harness` (per-validator, 14x slower, identical output) |
| `--fee-recipient`, `--graffiti`, `--no-json` | |

Logging follows `RUST_LOG`.

## Output

```
out/
  blocks/<slot>.ssz       signed beacon blocks (+ .json)
  envelopes/<slot>.ssz    signed payload envelopes, Gloas (+ .json)
  payloads/<slot>.json    engine_newPayload request per slot
  anchor/                 last finalized checkpoint: block, state, envelope, anchor.json
  interchange.json        EIP-3076 slashing protection, one record per key
  attesters.json          attesters of the last 256 slots, to cut the interchange earlier
  forkchoice.json         final engine_forkchoiceUpdated request
  manifest.json           per-block metadata
  summary.json            head, finality, EL hashes, watermarks, timings
  progress.json           rewritten at each epoch boundary
```

Post `blocks/` (and `envelopes/`) to beacon nodes in slot order, or start them from `anchor/` and feed their execution clients `payloads/` up to it. Import `interchange.json` into the validator clients before they start.

## Forks

Fulu and Gloas (from genesis or a later epoch), tested with Lighthouse, Prysm, Teku, Nimbus, Lodestar and Grandine on the `mainnet` preset. `minimal` is handled but untested. Electra payload export exists but is untested. Nothing earlier.

## Images

[`ghcr.io/ethpandaops/timeskip`](https://github.com/ethpandaops/timeskip/pkgs/container/timeskip), `linux/amd64` and `linux/arm64`, entrypoint `timeskip`, with `python3` for ethereum-package's scripts.

| Tag | |
|---|---|
| `latest`, `<sha7>` | master, against Lighthouse `unstable` |
| `<ref>-<sha7>` | manual builds against another Lighthouse ref |
| `<version>` | releases |

## Building

timeskip is a crate overlaid onto a Lighthouse checkout, since it needs Lighthouse's workspace patches. Requires a stable Rust toolchain, `cmake` and `clang`.

```sh
./scripts/build.sh                                 # clones sigp/lighthouse@unstable into ./lighthouse
./scripts/build.sh --ref <ref>                     # another Lighthouse ref
./scripts/build.sh --lighthouse-dir ../lighthouse  # existing checkout (gets modified)
./scripts/build.sh --prepare-only                  # overlay only
docker build --build-arg LIGHTHOUSE_REF=unstable -t timeskip .
```

The binary is `lighthouse/target/release/timeskip`. Edit the crate in `timeskip/`; every build copies it again. `patches/` holds optional performance patches for Lighthouse, skipped when they don't apply. CI builds against `unstable` on every PR and daily.

## Limitations

- Blocks carry no transactions or blobs.
- Payloads are checked only by the execution client that built them. reth 2.8.0 accepts Gloas payloads when genesis lacks the EIP-8282 contracts; geth rejects them.
- The final head's duties are left unsigned so validators can sign them live.
- A Gloas genesis builds slot 1 on an empty parent payload; the spec leaves this undefined, all six clients accept it.
- Don't cross a fork inside the generated history: Lighthouse and Grandine re-gossip API-received pre-fork blocks on the post-fork topic and Prysm bans them. Use `until` to stop before the fork.

Client quirks the package works around: Teku starts no gossip after API-only import (restart its beacon node); Prysm sends forkchoice updates out of order after envelopes (wait for its EL head); Grandine 3.0.0 lags one payload under Gloas and only accepts GET on `/eth/v4/validator/blocks`.

## License

[GPL-3.0](LICENSE). Built on [Lighthouse](https://github.com/sigp/lighthouse) (Apache-2.0).
