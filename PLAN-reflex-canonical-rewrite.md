# Plan: Rewrite reflex-canonical/src/lib.rs

## What exists today
- `CanonicalWriter` with lifetime-based `WriterSink<'a>` enum (Buffer or Hasher)
- `CanonicalEncode` trait + impls for primitives, String, Vec, Option, BTreeMap, Digest
- `content_id` with `&[u8]` domain
- `wrap_envelope` / `read_envelope_header` with complex 8-byte magic, schema_name, schema_version, digest fields
- No `HashMap` impl, no `write_f32_canonical` (just `write_f32`)

## What the user wants
1. **Method renames**: `write_u32` → `write_u32_le`, `write_u64` → `write_u64_le`, `write_byte_slice` → `write_bytes`, add `write_f32_canonical`
2. **`content_id` signature**: domain becomes `&'static [u8]`
3. **Add `CanonicalEncode` for `HashMap<K,V>`** (sorted via BTreeMap conversion)
4. **Simplified `EnvelopeHeader`**: `magic: [u8; 4]`, `schema_domain: Vec<u8>`, `payload_length: u32`
5. **`read_envelope_header(data: &[u8]) -> Result<EnvelopeHeader, CanonicalError>`** — no longer returns payload slice or digest verification
6. **Keep `wrap_envelope` and tests working** (tests use current API so they'll need updating)

## Changes to make

### 1. Rename writer methods
- `write_u32` → `write_u32_le` (add `write_u32` as alias if needed, or just rename)
- `write_u64` → `write_u64_le`
- `write_byte_slice` → `write_bytes`
- `write_f32` → `write_f32_canonical`
- `write_f64` → `write_f64_canonical`

### 2. Update CanonicalEncode impls
- All impls call the renamed methods
- Add `impl CanonicalEncode for HashMap<K, V>` that sorts keys via BTreeMap

### 3. Simplify EnvelopeHeader
```rust
pub struct EnvelopeHeader {
    pub magic: [u8; 4],
    pub schema_domain: Vec<u8>,
    pub payload_length: u32,
}
```

### 4. Rewrite read_envelope_header
- Simple: parse 4-byte magic, then length-prefixed domain, then u32 payload_length
- No digest verification in this function

### 5. Update content_id
- Domain parameter: `&'static [u8]`

### 6. Update wrap_envelope to match new magic (4 bytes not 8)
- ENVELOPE_MAGIC becomes `[u8; 4]`

### 7. Update tests to match new API

## Files to modify
- `/Users/ryanwible/projects/reflex/crates/reflex-canonical/src/lib.rs`

## Verification
- `cargo check -p reflex-canonical`
