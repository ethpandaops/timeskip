<p align="center">
  <img src="assets/logo.svg" width="112" alt="timeskip logo">
</p>

<h1 align="center">timeskip</h1>

<p align="center">Start Ethereum devnets already finalized.</p>

---

## How it works

A new devnet takes about 26 minutes to finalize. timeskip sets `genesis_time` far enough back that the chain would be finalized by now, past any fork epoch you asked for. Then it generates every slot since genesis, in seconds: a block per slot, signed attestations and sync messages, payloads executed on a real execution client. The clients load that history and validators start at the current slot, with slashing protection for everything timeskip signed.

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

Nodes reach the finalized head 40 to 90 seconds later. Not in an ethereum-package release yet.

| Key | Default | |
|---|---|---|
| `enabled` | `false` | |
| `until` | `finalized` at the last fork | `activated`, `justified`, `finalized`, a fork name, or `{fork\|epoch, state}` |
| `history` | `checkpoint` | `checkpoint` sync or `full` block import |
| `image` | `ghcr.io/ethpandaops/timeskip:latest` | |
| `el_image` | `ghcr.io/paradigmxyz/reth:latest` | private EL building the payloads |

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

Use a fresh, peerless EL on the same genesis and JWT. Gloas needs `engine_forkchoiceUpdatedV4`, `engine_getPayloadV6` and `engine_newPayloadV5` (reth 2.8.0 works).

| Flag | |
|---|---|
| `--testnet-dir` | `config.yaml` and `genesis.ssz` |
| `--mnemonic` | or `TIMESKIP_MNEMONIC` |
| `--validator-count` | must match genesis |
| `--engine-url`, `--jwt-secret` | the EL |
| `--output-dir` | see [Output](#output) |
| `--until-slot`, `--until-finalized-epoch` | stop condition (default: wall clock) |
| `--ahead-slots` | run ahead, to publish at slot time |
| `--max-slot` | hard cap, e.g. before a live fork |
| `--signing` | `summed` (default) or `harness`: 14x slower, same output |
| `--fee-recipient`, `--graffiti` | set on every block |
| `--no-json` | skip `.json` copies |

Logging follows `RUST_LOG`.

## Output

```
out/
  blocks/<slot>.ssz       signed blocks (+ .json)
  envelopes/<slot>.ssz    Gloas payload envelopes (+ .json)
  payloads/<slot>.json    engine_newPayload request
  anchor/                 finalized checkpoint: block, state, envelope, anchor.json
  interchange.json        EIP-3076 slashing protection, one record per key
  attesters.json          last 256 slots' attesters, for an earlier interchange cut
  forkchoice.json         final engine_forkchoiceUpdated request
  manifest.json           per-block metadata
  summary.json            head, finality, EL hashes, watermarks, timings
  progress.json           rewritten each epoch
```

Post `blocks/` (and `envelopes/`) to beacon nodes in slot order, or start them from `anchor/` and feed their ELs `payloads/` up to it. Validators import `interchange.json` before starting.

## Forks

Fulu and Gloas, from genesis or a later epoch, `mainnet` preset. Tested with Lighthouse, Prysm, Teku, Nimbus, Lodestar and Grandine. Untested: `minimal`, Electra payload export. Nothing older.

## Images

[`ghcr.io/ethpandaops/timeskip`](https://github.com/ethpandaops/timeskip/pkgs/container/timeskip): `linux/amd64` and `linux/arm64`, entrypoint `timeskip`, with `python3` for ethereum-package.

- `latest`, `<sha7>`: master, against Lighthouse `unstable`
- `<ref>-<sha7>`: manual build against another Lighthouse ref
- `<version>`: release

## Building

Overlaid onto a Lighthouse checkout for its workspace patches. Needs stable Rust, `cmake`, `clang`.

```sh
./scripts/build.sh                                 # clone sigp/lighthouse@unstable
./scripts/build.sh --ref <ref>                     # another Lighthouse ref
./scripts/build.sh --lighthouse-dir ../lighthouse  # existing checkout, modified
./scripts/build.sh --prepare-only                  # overlay only
docker build --build-arg LIGHTHOUSE_REF=unstable -t timeskip .
```

Binary: `lighthouse/target/release/timeskip`. Edit `timeskip/`; each build re-copies it. `patches/`: optional Lighthouse speedups, skipped if they don't apply. CI builds against `unstable` per PR and daily.

## Limitations

- No transactions or blobs.
- Only the EL that built a payload checks it. Gloas without the EIP-8282 genesis contracts: reth 2.8.0 accepts, geth rejects.
- The final head's duties are left to the live validators.
- Gloas at genesis: slot 1 builds on an empty parent payload. Undefined in the spec; all six clients accept it.
- Keep forks out of generated history (`until`): Lighthouse and Grandine re-gossip API-received pre-fork blocks on the post-fork topic; Prysm bans them.

Package workarounds:

- Teku: no gossip after API-only import; restart its beacon node.
- Prysm: out-of-order forkchoice updates after envelopes; wait for its EL head.
- Grandine 3.0.0: one payload behind under Gloas; `/eth/v4/validator/blocks` is GET only.

## License

[GPL-3.0](LICENSE). Built on [Lighthouse](https://github.com/sigp/lighthouse) (Apache-2.0).
