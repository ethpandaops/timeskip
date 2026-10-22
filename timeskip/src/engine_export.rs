use anyhow::{Result, anyhow, bail};
use execution_layer::NewPayloadRequest;
use execution_layer::json_structures::JsonExecutionPayload;
use serde_json::{Value, json};
use state_processing::per_block_processing::deneb::kzg_commitment_to_versioned_hash;
use types::{
    EthSpec, ExecutionBlockHash, ExecutionRequestsRef, SignedBeaconBlock,
    SignedExecutionPayloadEnvelope, VersionedHash,
};

/// The `engine_newPayload` JSON-RPC request that imports `block`'s payload into an EL.
pub fn new_payload_request<E: EthSpec>(block: &SignedBeaconBlock<E>) -> Result<Value> {
    let slot = block.slot().as_u64();
    let request = NewPayloadRequest::try_from(block.message())
        .map_err(|e| anyhow!("block {slot} has no payload request: {e:?}"))?;

    let (payload, versioned_hashes, parent_beacon_block_root, requests) = match request {
        NewPayloadRequest::Electra(request) => (
            JsonExecutionPayload::Electra(
                request
                    .execution_payload
                    .clone()
                    .try_into()
                    .map_err(|e| anyhow!("payload {slot}: {e:?}"))?,
            ),
            request.versioned_hashes,
            request.parent_beacon_block_root,
            ExecutionRequestsRef::Electra(request.execution_requests),
        ),
        NewPayloadRequest::Fulu(request) => (
            JsonExecutionPayload::Fulu(
                request
                    .execution_payload
                    .clone()
                    .try_into()
                    .map_err(|e| anyhow!("payload {slot}: {e:?}"))?,
            ),
            request.versioned_hashes,
            request.parent_beacon_block_root,
            ExecutionRequestsRef::Electra(request.execution_requests),
        ),
        _ => bail!("newPayload export supports Electra and Fulu blocks only (slot {slot})"),
    };

    Ok(json!({
        "jsonrpc": "2.0",
        "id": slot,
        "method": "engine_newPayloadV4",
        "params": [
            payload,
            versioned_hashes,
            parent_beacon_block_root,
            requests.get_execution_requests_list(),
        ],
    }))
}

/// The `engine_newPayloadV5` request for a Gloas envelope, as a CL sends it on import.
pub fn envelope_new_payload_request<E: EthSpec>(
    block: &SignedBeaconBlock<E>,
    envelope: &SignedExecutionPayloadEnvelope<E>,
) -> Result<Value> {
    let slot = envelope.slot().as_u64();
    let bid = block
        .message()
        .body()
        .signed_execution_payload_bid()
        .map_err(|e| anyhow!("block {slot} has no bid: {e:?}"))?;
    let versioned_hashes: Vec<VersionedHash> = bid
        .message()
        .blob_kzg_commitments()
        .iter()
        .map(kzg_commitment_to_versioned_hash)
        .collect();
    let payload = JsonExecutionPayload::Gloas(
        envelope
            .message
            .payload
            .clone()
            .try_into()
            .map_err(|e| anyhow!("payload {slot}: {e:?}"))?,
    );

    Ok(json!({
        "jsonrpc": "2.0",
        "id": slot,
        "method": "engine_newPayloadV5",
        "params": [
            payload,
            versioned_hashes,
            envelope.message.parent_beacon_block_root,
            ExecutionRequestsRef::Gloas(&envelope.message.execution_requests)
                .get_execution_requests_list(),
        ],
    }))
}

/// The head-only `engine_forkchoiceUpdated` that makes `head` canonical: V4 for a Gloas head.
pub fn forkchoice_request(
    head: ExecutionBlockHash,
    safe: ExecutionBlockHash,
    finalized: ExecutionBlockHash,
    gloas: bool,
) -> Value {
    let method = if gloas {
        "engine_forkchoiceUpdatedV4"
    } else {
        "engine_forkchoiceUpdatedV3"
    };
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": [
            {
                "headBlockHash": head,
                "safeBlockHash": safe,
                "finalizedBlockHash": finalized,
            },
            null,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forkchoice_request_uses_v4_once_the_head_is_gloas() {
        let hash = ExecutionBlockHash::repeat_byte(1);
        let fulu = forkchoice_request(hash, hash, hash, false);
        let gloas = forkchoice_request(hash, hash, hash, true);
        assert_eq!(fulu["method"], "engine_forkchoiceUpdatedV3");
        assert_eq!(gloas["method"], "engine_forkchoiceUpdatedV4");
        assert_eq!(gloas["params"][1], Value::Null);
        assert_eq!(gloas["params"].as_array().map(Vec::len), Some(2));
    }
}
