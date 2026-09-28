# Plan: SAPIENT Codec for OpenTrack

## Goal

Add SAPIENT (BSI Flex 335) as a message codec so tracks delivered over MQTT or
TCP can be ingested, decoded, mapped to the canonical observation schema, and
correlated like any other source.

## Key Findings

### Transports (already covered — no new code)

- **MQTT**: `crate::ot-source/src/transport.rs` already implements
  `TransportConfig::Mqtt` using `rumqttc`. Topics carry metadata onto each
  `Frame` via `frame.meta` (the `topic` key). Reused as-is.
- **TCP**: Already supported. SAPIENT's wire format is a 4-byte
  little-endian length prefix followed by a protobuf payload. This matches the
  existing `Framing::LengthPrefix { width: U32, endian: Little }` in
  `crate::ot-source/src/frame.rs` — no custom framing needed.

### SAPIENT Protocol (`sapient-rs` v0.2.0)

- Published at `https://crates.io/crates/sapient-rs`, repo at
  `https://github.com/tom-mann-ironclad/sapient-bindings`.
- Feature flags: `v1_0` and `v2_0` (default). Both expose
  `FILE_DESCRIPTOR_SET_BYTES`.
- Core message: `SapientMessage` with `timestamp`, `node_id`,
  `destination_id`, `content` (oneof), `additional_information`.
- Track-bearing content type: `DetectionReport` — carries `object_id`,
  `location_oneof` (GeoPoint Lat/Lng or RangeBearing), `velocity_oneof`
  (ENU), `classifications`, `behaviours`, `signals`.
- `sapient-rs` generates `prost` structs that do **not** implement
  `serde::Serialize`/`Deserialize`. Decoding therefore requires runtime
  protobuf reflection.

### Codec Strategy

Use `prost-reflect` with `sapient-rs`'s `FILE_DESCRIPTOR_SET_BYTES` to decode
raw protobuf bytes into `prost_reflect::DynamicMessage`, then convert to
`serde_json::Value` for the existing mapping pipeline.

```
raw bytes ──► prost-reflect DynamicMessage ──► serde_json::Value ──► Mapping
```

The `Codec::decode` trait method receives a `&Frame` (containing `bytes` +
`meta`) and returns `Vec<Value>`. The SAPIENT codec decodes the outer
`SapientMessage`, extracts the `content` oneof variant, and emits one or more
records depending on the message type.

## Design

### New crate: `crates/ot-sapient/`

Follows the existing per-domain crate pattern (`ot-core`, `ot-nats`,
`ot-source`, `ot-store`).

#### `Cargo.toml`

```toml
[package]
name = "ot-sapient"
description = "SAPIENT (BSI Flex 335) codec for OpenTrack"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
ot-core.workspace = true
ot-source.workspace = true
bytes.workspace = true
prost-reflect = "0.16"
serde_json.workspace = true
thiserror.workspace = true
tracing.workspace = true

[dependencies.sapient-rs]
version = "0.2"
default-features = false
features = ["v2_0"]

[dev-dependencies]
chrono.workspace = true

[lints]
workspace = true
```

Shared workspace deps to add to `Cargo.toml`:
- `prost-reflect = "0.16"`

#### `src/lib.rs`

Re-export the public codec type and error.

#### `src/codec.rs`

Implements the SAPIENT decoder:

1. **Descriptor pool**: Built once from `sapient_rs::FILE_DESCRIPTOR_SET_BYTES`
   via `prost_reflect::DescriptorPool`.
2. **`SapientCodec`**: holds the descriptor pool and the top-level message
   descriptor (`SapientMessage`).
3. **`decode(&Frame) -> Result<Vec<Value>, CodecError>`**:
   - Decodes `frame.bytes` as a `SapientMessage` dynamic message.
   - Reads the `content` oneof to determine the variant
     (`DetectionReport`, `Registration`, `StatusReport`, etc.).
   - Converts the dynamic message to `serde_json::Value` via
     `prost_reflect`'s built-in JSON conversion
     (`DynamicMessage::to_prost_reflect_json` or manual walk).
   - Merges `frame.meta` into each record under `_frame` (consistent with the
     JSON/XML codecs).
   - Emits one record per logical track/report.

#### Error type

```rust
#[derive(Debug, thiserror::Error)]
pub enum SapientError {
    #[error("invalid SAPIENT protobuf: {0}")]
    Decode(#[from] prost_reflect::DecodeError),
    #[error("missing SapientMessage content oneof")]
    MissingContent,
    #[error("unsupported content type: {0}")]
    UnsupportedContentType(String),
}
```

### Integration Points

1. **Workspace**: Add `ot-sapient` to `members = ["crates/*"]` (auto-discovered
   by glob). Add `prost-reflect` to `[workspace.dependencies]`.
2. **Codec config**: Extend `ot-source`'s `CodecConfig` enum with a
   `Sapient` variant, or register `ot-sapient` as a plugin. Since codecs are
   currently embedded in `ot-source`, the simplest path is:
   - Add `CodecConfig::Sapient` to `ot-source/src/codec.rs`.
   - In `Codec::decode`, delegate to `ot_sapient::SapientCodec` when the
     variant is selected.
3. **Source spec**: No changes to `SourceSpec` or `PipelineSpec` — the codec
   plugs into the existing `Pipeline` abstraction transparently.
4. **Probe/config DB**: No changes needed initially; SAPIENT sources are
   configured through the standard `SourceSpec` with
   `codec.type = "sapient"`.

### Configuration Example

```json
{
  "id": "my-sapient-feed",
  "transport": {
    "type": "mqtt",
    "broker": "tcp://localhost:1883",
    "topics": ["sapt/01/detections"]
  },
  "framing": {
    "type": "message"
  },
  "codec": {
    "type": "sapient"
  },
  "pipeline": { ... }
}
```

Or over TCP:

```json
{
  "transport": {
    "type": "tcp",
    "host": "0.0.0.0",
    "port": 5678,
    "framing": {
      "type": "length_prefix",
      "width": "u32",
      "endian": "little"
    }
  },
  "codec": { "type": "sapient" }
}
```

## Implementation Steps

1. Create `crates/ot-sapient/` scaffold (`Cargo.toml`, `src/lib.rs`,
   `src/codec.rs`).
2. Implement protobuf-to-`Value` decoding using `prost-reflect`.
3. Wire `CodecConfig::Sapient` into `ot-source/src/codec.rs`.
4. Add unit tests with sample SAPIENT protobuf fixtures.
5. Build and verify compilation under workspace lints
   (`unsafe_code = forbid`, `dbg_macro = warn`).

## Testing Strategy

- Unit tests in `ot-sapient/src/codec.rs` with hand-crafted protobuf bytes
  for `SapientMessage` wrapping a `DetectionReport`.
- Round-trip test: encode a `DynamicMessage` → decode → verify JSON structure.
- Verify `frame.meta` propagation (e.g., MQTT topic) reaches `_frame`.

## Alignment with Phase 5 Roadmap

Item: *"Protobuf from `.proto`, brokers, WebAssembly plugins"*

This work establishes the SAPIENT codec as a reusable building block. Future
extensions:
- WASM plugin loading for arbitrary protobuf schemas.
- Broker-side filtering (MQTT topic subscriptions per message type).
- Schema evolution support across SAPIENT v1/v2.
