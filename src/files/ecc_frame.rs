use std::io::{Read, Write};

use anyhow::{Context, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;

const FRAME_MAGIC: &[u8; 4] = b"MJRP";
const MAX_FRAME_PAYLOAD: usize = 64 * 1024 * 1024;
const ECC_TARGET_SHARD_SIZE: usize = 4096;
const ECC_MIN_DATA_SHARDS: usize = 2;
const ECC_MAX_DATA_SHARDS: usize = 32;
const ECC_PARITY_SHARDS: usize = 2;

struct EccLayout {
    data_shards: usize,
    parity_shards: usize,
    shard_size: usize,
}

impl EccLayout {
    fn for_payload(payload_len: usize) -> Result<Self> {
        if payload_len > MAX_FRAME_PAYLOAD {
            anyhow::bail!(
                "ecc frame payload too large: {} bytes (max {})",
                payload_len,
                MAX_FRAME_PAYLOAD
            );
        }
        let effective = payload_len.max(1);
        let mut data_shards = effective.div_ceil(ECC_TARGET_SHARD_SIZE);
        data_shards = data_shards.clamp(ECC_MIN_DATA_SHARDS, ECC_MAX_DATA_SHARDS);
        let shard_size = effective.div_ceil(data_shards).max(1);
        Ok(Self {
            data_shards,
            parity_shards: ECC_PARITY_SHARDS,
            shard_size,
        })
    }

}

pub fn write_frame(writer: &mut impl Write, payload: &[u8]) -> Result<()> {
    let layout = EccLayout::for_payload(payload.len())?;
    let payload_len = u32::try_from(payload.len()).context("payload size exceeds u32")?;
    let payload_crc = super::crc::hash(payload);

    let mut padded = vec![0u8; layout.data_shards * layout.shard_size];
    padded[..payload.len()].copy_from_slice(payload);
    let mut shards: Vec<Vec<u8>> = padded
        .chunks(layout.shard_size)
        .map(|chunk| chunk.to_vec())
        .collect();
    for _ in 0..layout.parity_shards {
        shards.push(vec![0u8; layout.shard_size]);
    }

    let codec = ReedSolomon::new(layout.data_shards, layout.parity_shards)
        .context("failed building reed-solomon codec")?;
    codec
        .encode(shards.as_mut_slice())
        .context("failed encoding parity shards")?;

    let shard_crcs: Vec<u32> = shards
        .iter()
        .map(|shard| super::crc::hash(shard.as_slice()))
        .collect();

    writer.write_all(FRAME_MAGIC)?;
    writer.write_all(&payload_len.to_le_bytes())?;
    writer.write_all(&payload_crc.to_le_bytes())?;
    writer.write_all(&(layout.data_shards as u16).to_le_bytes())?;
    writer.write_all(&(layout.parity_shards as u16).to_le_bytes())?;
    writer.write_all(&(layout.shard_size as u32).to_le_bytes())?;
    for crc in &shard_crcs {
        writer.write_all(&crc.to_le_bytes())?;
    }
    for shard in &shards {
        writer.write_all(shard.as_slice())?;
    }
    Ok(())
}

pub fn read_frame(reader: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut magic = [0u8; 4];
    match reader.read_exact(&mut magic) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    }
    if &magic != FRAME_MAGIC {
        anyhow::bail!("invalid ecc frame magic: {:?}", magic);
    }

    let mut buf4 = [0u8; 4];
    let mut buf2 = [0u8; 2];

    reader.read_exact(&mut buf4)?;
    let payload_len = u32::from_le_bytes(buf4) as usize;
    if payload_len > MAX_FRAME_PAYLOAD {
        anyhow::bail!("ecc frame payload too large: {}", payload_len);
    }

    reader.read_exact(&mut buf4)?;
    let payload_crc = u32::from_le_bytes(buf4);

    reader.read_exact(&mut buf2)?;
    let data_shards = u16::from_le_bytes(buf2) as usize;
    reader.read_exact(&mut buf2)?;
    let parity_shards = u16::from_le_bytes(buf2) as usize;
    reader.read_exact(&mut buf4)?;
    let shard_size = u32::from_le_bytes(buf4) as usize;

    if !(ECC_MIN_DATA_SHARDS..=ECC_MAX_DATA_SHARDS).contains(&data_shards) {
        anyhow::bail!("invalid data_shards: {}", data_shards);
    }
    if parity_shards != ECC_PARITY_SHARDS {
        anyhow::bail!("invalid parity_shards: {}", parity_shards);
    }
    if shard_size == 0 {
        anyhow::bail!("invalid shard_size: 0");
    }

    let total_shards = data_shards + parity_shards;
    let mut expected_crcs = Vec::with_capacity(total_shards);
    for _ in 0..total_shards {
        reader.read_exact(&mut buf4)?;
        expected_crcs.push(u32::from_le_bytes(buf4));
    }

    let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(total_shards);
    let mut corrupted = 0usize;
    for expected_crc in &expected_crcs {
        let mut shard = vec![0u8; shard_size];
        reader.read_exact(&mut shard)?;
        let actual_crc = super::crc::hash(shard.as_slice());
        if actual_crc == *expected_crc {
            shards.push(Some(shard));
        } else {
            corrupted += 1;
            shards.push(None);
        }
    }

    if corrupted > parity_shards {
        anyhow::bail!(
            "uncorrectable frame corruption: {} damaged shards (parity={})",
            corrupted,
            parity_shards
        );
    }

    if corrupted > 0 {
        let codec = ReedSolomon::new(data_shards, parity_shards)
            .context("failed building reed-solomon codec")?;
        codec
            .reconstruct(shards.as_mut_slice())
            .context("failed reconstructing corrupted shards")?;
    }

    let mut payload = Vec::with_capacity(data_shards * shard_size);
    for (idx, shard_opt) in shards.iter().enumerate() {
        if idx >= data_shards {
            break;
        }
        let shard = shard_opt
            .as_ref()
            .context("missing shard after reconstruction")?;
        payload.extend_from_slice(shard.as_slice());
    }
    payload.truncate(payload_len);

    let actual_crc = super::crc::hash(payload.as_slice());
    if actual_crc != payload_crc {
        anyhow::bail!(
            "payload checksum mismatch: expected {:08x}, got {:08x}",
            payload_crc,
            actual_crc
        );
    }

    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::{read_frame, write_frame};
    use std::io::Cursor;

    #[test]
    fn roundtrip_small_payload() {
        let payload = b"hello ecc frame";
        let mut buf = Vec::new();
        write_frame(&mut buf, payload).expect("write");
        let mut cursor = Cursor::new(buf);
        let decoded = read_frame(&mut cursor).expect("read").expect("some");
        assert_eq!(decoded.as_slice(), payload);
    }

    #[test]
    fn roundtrip_empty_payload() {
        let payload = b"";
        let mut buf = Vec::new();
        write_frame(&mut buf, payload).expect("write");
        let mut cursor = Cursor::new(buf);
        let decoded = read_frame(&mut cursor).expect("read").expect("some");
        assert!(decoded.is_empty());
    }

    #[test]
    fn recovers_shard_corruption() {
        let payload = b"test data for ecc recovery that is longer than a few bytes to ensure multiple shards";
        let mut buf = Vec::new();
        write_frame(&mut buf, payload).expect("write");

        let header_len = 4 + 4 + 4 + 2 + 2 + 4;
        let data_shards = u16::from_le_bytes([buf[12], buf[13]]) as usize;
        let parity_shards = u16::from_le_bytes([buf[14], buf[15]]) as usize;
        let total_shards = data_shards + parity_shards;
        let shard_size = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]) as usize;
        let shard_start = header_len + (total_shards * 4);
        buf[shard_start + shard_size / 2] ^= 0xFF;

        let mut cursor = Cursor::new(buf);
        let decoded = read_frame(&mut cursor).expect("read").expect("some");
        assert_eq!(decoded.as_slice(), payload);
    }

    #[test]
    fn eof_returns_none() {
        let buf: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(buf);
        let result = read_frame(&mut cursor).expect("read");
        assert!(result.is_none());
    }
}
