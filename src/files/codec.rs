use anyhow::{Context, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::path::Path;

const BINARY_MAGIC: &[u8] = b"MJFEC001";
const MAX_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;
const ECC_TARGET_SHARD_SIZE: usize = 4096;
const ECC_MIN_DATA_SHARDS: usize = 2;
const ECC_MAX_DATA_SHARDS: usize = 32;
const ECC_PARITY_SHARDS: usize = 2;

const PAYLOAD_LEN_BYTES: usize = std::mem::size_of::<u32>();
const DATA_SHARDS_BYTES: usize = std::mem::size_of::<u16>();
const PARITY_SHARDS_BYTES: usize = std::mem::size_of::<u16>();
const SHARD_SIZE_BYTES: usize = std::mem::size_of::<u32>();
const PAYLOAD_CHECKSUM_BYTES: usize = std::mem::size_of::<u32>();
const SHARD_CHECKSUM_BYTES: usize = std::mem::size_of::<u32>();

const PAYLOAD_LEN_OFFSET: usize = BINARY_MAGIC.len();
const DATA_SHARDS_OFFSET: usize = PAYLOAD_LEN_OFFSET + PAYLOAD_LEN_BYTES;
const PARITY_SHARDS_OFFSET: usize = DATA_SHARDS_OFFSET + DATA_SHARDS_BYTES;
const SHARD_SIZE_OFFSET: usize = PARITY_SHARDS_OFFSET + PARITY_SHARDS_BYTES;
const PAYLOAD_CHECKSUM_OFFSET: usize = SHARD_SIZE_OFFSET + SHARD_SIZE_BYTES;
const HEADER_LEN: usize = PAYLOAD_CHECKSUM_OFFSET + PAYLOAD_CHECKSUM_BYTES;

struct EccLayout {
    data_shards: usize,
    parity_shards: usize,
    shard_size: usize,
}

impl EccLayout {
    fn for_payload_len(payload_len: usize) -> Result<Self> {
        if payload_len > MAX_PAYLOAD_BYTES {
            anyhow::bail!(
                "encoded state payload too large: {} bytes (max {})",
                payload_len,
                MAX_PAYLOAD_BYTES
            );
        }
        let effective_payload = payload_len.max(1);
        let mut data_shards = effective_payload.div_ceil(ECC_TARGET_SHARD_SIZE);
        data_shards = data_shards.clamp(ECC_MIN_DATA_SHARDS, ECC_MAX_DATA_SHARDS);
        let shard_size = effective_payload.div_ceil(data_shards).max(1);
        Ok(Self {
            data_shards,
            parity_shards: ECC_PARITY_SHARDS,
            shard_size,
        })
    }

    fn from_header(data_shards: usize, parity_shards: usize, shard_size: usize) -> Result<Self> {
        if !(ECC_MIN_DATA_SHARDS..=ECC_MAX_DATA_SHARDS).contains(&data_shards) {
            anyhow::bail!(
                "invalid data_shards in state frame: {} (allowed {}..={})",
                data_shards,
                ECC_MIN_DATA_SHARDS,
                ECC_MAX_DATA_SHARDS
            );
        }
        if parity_shards != ECC_PARITY_SHARDS {
            anyhow::bail!(
                "invalid parity_shards in state frame: {} (expected {})",
                parity_shards,
                ECC_PARITY_SHARDS
            );
        }
        if shard_size == 0 {
            anyhow::bail!("invalid shard_size in state frame: 0");
        }
        Ok(Self {
            data_shards,
            parity_shards,
            shard_size,
        })
    }

    fn total_shards(&self) -> usize {
        self.data_shards + self.parity_shards
    }

    fn checksum_table_len(&self) -> Result<usize> {
        self.total_shards()
            .checked_mul(SHARD_CHECKSUM_BYTES)
            .context("state frame checksum table overflow")
    }

    fn shards_len(&self) -> Result<usize> {
        self.total_shards()
            .checked_mul(self.shard_size)
            .context("state frame shard byte length overflow")
    }

    fn encoded_len(&self) -> Result<usize> {
        let checksum_table_len = self.checksum_table_len()?;
        let shards_len = self.shards_len()?;
        HEADER_LEN
            .checked_add(checksum_table_len)
            .and_then(|value| value.checked_add(shards_len))
            .context("state frame total length overflow")
    }
}

pub struct StateCodec;

impl StateCodec {
    pub fn max_state_bytes() -> usize {
        EccLayout::for_payload_len(MAX_PAYLOAD_BYTES)
            .and_then(|layout| layout.encoded_len())
            .unwrap_or(usize::MAX)
    }

    pub fn read_decode_binary<T>(path: &Path) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed reading state metadata {}", path.display()))?;
        let size = metadata.len() as usize;
        if size > Self::max_state_bytes() {
            anyhow::bail!(
                "state file too large: {} bytes (max {}) at {}",
                size,
                Self::max_state_bytes(),
                path.display()
            );
        }
        let bytes = fs::read(path)
            .with_context(|| format!("failed reading state file {}", path.display()))?;
        Self::decode_binary(bytes.as_slice())
    }

    pub fn encode_binary<T>(value: &T) -> Result<Vec<u8>>
    where
        T: Serialize,
    {
        let payload = bincode::serde::encode_to_vec(value, bincode::config::standard())
            .context("failed bincode state encoding")?;
        let layout = EccLayout::for_payload_len(payload.len())?;
        let payload_len = u32::try_from(payload.len()).context("payload size exceeds u32")?;
        let payload_checksum = crc32fast::hash(payload.as_slice());

        let total_shards = layout.total_shards();
        let mut shards = Vec::<Vec<u8>>::with_capacity(total_shards);
        let mut padded = vec![0u8; layout.data_shards * layout.shard_size];
        padded[..payload.len()].copy_from_slice(payload.as_slice());
        for chunk in padded.chunks(layout.shard_size) {
            shards.push(chunk.to_vec());
        }
        for _ in 0..layout.parity_shards {
            shards.push(vec![0u8; layout.shard_size]);
        }

        let codec = ReedSolomon::new(layout.data_shards, layout.parity_shards)
            .context("failed building reed-solomon codec")?;
        codec
            .encode(shards.as_mut_slice())
            .context("failed encoding reed-solomon parity shards")?;

        let shard_checksums = shards
            .iter()
            .map(|shard| crc32fast::hash(shard.as_slice()))
            .collect::<Vec<_>>();

        let encoded_len = layout.encoded_len()?;
        let mut encoded = Vec::with_capacity(encoded_len);
        encoded.extend_from_slice(BINARY_MAGIC);
        encoded.extend_from_slice(&payload_len.to_le_bytes());
        encoded.extend_from_slice(
            &u16::try_from(layout.data_shards)
                .context("data shard count exceeds u16")?
                .to_le_bytes(),
        );
        encoded.extend_from_slice(
            &u16::try_from(layout.parity_shards)
                .context("parity shard count exceeds u16")?
                .to_le_bytes(),
        );
        encoded.extend_from_slice(
            &u32::try_from(layout.shard_size)
                .context("shard size exceeds u32")?
                .to_le_bytes(),
        );
        encoded.extend_from_slice(&payload_checksum.to_le_bytes());
        for checksum in &shard_checksums {
            encoded.extend_from_slice(&checksum.to_le_bytes());
        }
        for shard in &shards {
            encoded.extend_from_slice(shard.as_slice());
        }
        Ok(encoded)
    }

    pub fn decode_binary<T>(bytes: &[u8]) -> Result<T>
    where
        T: DeserializeOwned,
    {
        if !bytes.starts_with(BINARY_MAGIC) {
            anyhow::bail!("unsupported state format; expected current ECC binary framing");
        }
        if bytes.len() < HEADER_LEN {
            anyhow::bail!(
                "invalid state frame: {} bytes (header {})",
                bytes.len(),
                HEADER_LEN
            );
        }

        let payload_len = read_u32(bytes, PAYLOAD_LEN_OFFSET, "payload length")? as usize;
        if payload_len > MAX_PAYLOAD_BYTES {
            anyhow::bail!(
                "binary state payload exceeds max size: {} bytes (max {})",
                payload_len,
                MAX_PAYLOAD_BYTES
            );
        }
        let data_shards = read_u16(bytes, DATA_SHARDS_OFFSET, "data_shards")? as usize;
        let parity_shards = read_u16(bytes, PARITY_SHARDS_OFFSET, "parity_shards")? as usize;
        let shard_size = read_u32(bytes, SHARD_SIZE_OFFSET, "shard_size")? as usize;
        let payload_checksum = read_u32(bytes, PAYLOAD_CHECKSUM_OFFSET, "payload_checksum")?;

        let layout = EccLayout::from_header(data_shards, parity_shards, shard_size)?;
        let checksum_table_len = layout.checksum_table_len()?;
        let shards_len = layout.shards_len()?;
        let expected_total_len = HEADER_LEN
            .checked_add(checksum_table_len)
            .and_then(|value| value.checked_add(shards_len))
            .context("state frame total length overflow")?;
        if bytes.len() != expected_total_len {
            anyhow::bail!(
                "state frame length mismatch: expected {}, actual {}",
                expected_total_len,
                bytes.len()
            );
        }

        let checksum_start = HEADER_LEN;
        let shard_bytes_start = checksum_start + checksum_table_len;
        let mut expected_shard_checksums = Vec::<u32>::with_capacity(layout.total_shards());
        for idx in 0..layout.total_shards() {
            expected_shard_checksums.push(read_u32(
                bytes,
                checksum_start + (idx * SHARD_CHECKSUM_BYTES),
                "shard_checksum",
            )?);
        }

        let mut missing_shards = 0usize;
        let mut shards = Vec::<Option<Vec<u8>>>::with_capacity(layout.total_shards());
        for (idx, expected_checksum) in expected_shard_checksums.iter().enumerate() {
            let start = shard_bytes_start + (idx * layout.shard_size);
            let end = start + layout.shard_size;
            let shard = bytes[start..end].to_vec();
            let checksum = crc32fast::hash(shard.as_slice());
            if checksum == *expected_checksum {
                shards.push(Some(shard));
            } else {
                missing_shards = missing_shards.saturating_add(1);
                shards.push(None);
            }
        }
        if missing_shards > layout.parity_shards {
            anyhow::bail!(
                "uncorrectable state corruption: {} damaged shards with parity {}",
                missing_shards,
                layout.parity_shards
            );
        }

        let codec = ReedSolomon::new(layout.data_shards, layout.parity_shards)
            .context("failed building reed-solomon codec")?;
        codec
            .reconstruct(shards.as_mut_slice())
            .context("failed reconstructing corrupted state shards")?;

        let mut payload = Vec::<u8>::with_capacity(layout.data_shards * layout.shard_size);
        for (idx, expected_checksum) in expected_shard_checksums.iter().enumerate() {
            let shard = shards
                .get(idx)
                .and_then(Option::as_ref)
                .context("missing shard after reconstruction")?;
            let checksum = crc32fast::hash(shard.as_slice());
            if checksum != *expected_checksum {
                anyhow::bail!(
                    "reconstructed shard checksum mismatch at index {}: expected {:08x}, got {:08x}",
                    idx,
                    expected_checksum,
                    checksum
                );
            }
            if idx < layout.data_shards {
                payload.extend_from_slice(shard.as_slice());
            }
        }
        payload.truncate(payload_len);

        let decoded_payload_checksum = crc32fast::hash(payload.as_slice());
        if decoded_payload_checksum != payload_checksum {
            anyhow::bail!(
                "payload checksum mismatch after reconstruction: expected {:08x}, got {:08x}",
                payload_checksum,
                decoded_payload_checksum
            );
        }

        let (decoded, consumed) = bincode::serde::decode_from_slice::<T, _>(
            payload.as_slice(),
            bincode::config::standard(),
        )
        .context("failed bincode state decoding")?;
        if consumed != payload.len() {
            anyhow::bail!(
                "failed bincode state decoding: trailing bytes (consumed {}, total {})",
                consumed,
                payload.len()
            );
        }
        Ok(decoded)
    }
}

fn read_u16(bytes: &[u8], offset: usize, field: &str) -> Result<u16> {
    let end = offset + std::mem::size_of::<u16>();
    let raw = bytes
        .get(offset..end)
        .with_context(|| format!("missing {} field bytes", field))?;
    let value =
        <[u8; 2]>::try_from(raw).with_context(|| format!("invalid {} field bytes", field))?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize, field: &str) -> Result<u32> {
    let end = offset + std::mem::size_of::<u32>();
    let raw = bytes
        .get(offset..end)
        .with_context(|| format!("missing {} field bytes", field))?;
    let value =
        <[u8; 4]>::try_from(raw).with_context(|| format!("invalid {} field bytes", field))?;
    Ok(u32::from_le_bytes(value))
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use crate::files::codec::{
        read_u16, read_u32, StateCodec, BINARY_MAGIC, DATA_SHARDS_OFFSET, PARITY_SHARDS_OFFSET,
        SHARD_CHECKSUM_BYTES, SHARD_SIZE_OFFSET,
    };

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct DemoState {
        value: u64,
        label: String,
    }

    fn first_shard_offset_and_size(bytes: &[u8]) -> (usize, usize) {
        let data_shards = read_u16(bytes, DATA_SHARDS_OFFSET, "data_shards").expect("data shards");
        let parity_shards =
            read_u16(bytes, PARITY_SHARDS_OFFSET, "parity_shards").expect("parity shards");
        let shard_size =
            read_u32(bytes, SHARD_SIZE_OFFSET, "shard_size").expect("shard size") as usize;
        let total_shards = usize::from(data_shards) + usize::from(parity_shards);
        let checksum_table_len = total_shards
            .checked_mul(SHARD_CHECKSUM_BYTES)
            .expect("checksum table");
        let shard_start = super::HEADER_LEN + checksum_table_len;
        (shard_start, shard_size)
    }

    #[test]
    fn binary_roundtrip() {
        let state = DemoState {
            value: 9,
            label: "demo".to_string(),
        };
        let encoded = StateCodec::encode_binary(&state).expect("encode");
        let decoded: DemoState = StateCodec::decode_binary(encoded.as_slice()).expect("decode");
        assert_eq!(decoded, state);
    }

    #[test]
    fn rejects_binary_format() {
        let json = br#"{"value":7,"label":"unsupported"}"#;
        assert!(StateCodec::decode_binary::<DemoState>(json).is_err());
    }

    #[test]
    fn recovers_shard_corruption() {
        let state = DemoState {
            value: 42,
            label: "ecc-recover".to_string(),
        };
        let mut encoded = StateCodec::encode_binary(&state).expect("encode");
        assert!(encoded.starts_with(BINARY_MAGIC));
        let (shard_start, shard_size) = first_shard_offset_and_size(encoded.as_slice());
        encoded[shard_start + (shard_size / 2)] ^= 0x01;

        let decoded: DemoState = StateCodec::decode_binary(encoded.as_slice()).expect("decode");
        assert_eq!(decoded, state);
    }

    #[test]
    fn corruption_exceeds_parity() {
        let state = DemoState {
            value: 100,
            label: "ecc-fail".to_string(),
        };
        let mut encoded = StateCodec::encode_binary(&state).expect("encode");
        let (shard_start, shard_size) = first_shard_offset_and_size(encoded.as_slice());
        for idx in 0..3 {
            encoded[shard_start + (idx * shard_size)] ^= 0x01;
        }
        assert!(StateCodec::decode_binary::<DemoState>(encoded.as_slice()).is_err());
    }
}
