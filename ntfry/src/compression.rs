//! ZSTD-сжатие чанков. Поддержка пользовательского статического словаря
//! (user.dict); при его отсутствии — стандартное сжатие.

use crate::error::VaultError;
use crate::vfs::MAX_CHUNK_SIZE;

const LEVEL: i32 = 3;

pub struct Codec {
    dict: Option<Vec<u8>>,
}

impl Codec {
    pub fn new(dict: Option<Vec<u8>>) -> Self {
        Self { dict }
    }

    pub fn has_dict(&self) -> bool {
        self.dict.is_some()
    }

    pub fn compress(&self, data: &[u8]) -> Result<Vec<u8>, VaultError> {
        let err = |e: std::io::Error| VaultError::Compression(e.to_string());
        match &self.dict {
            Some(d) => {
                let mut c = zstd::bulk::Compressor::with_dictionary(LEVEL, d).map_err(err)?;
                c.compress(data).map_err(err)
            }
            None => zstd::bulk::compress(data, LEVEL).map_err(err),
        }
    }

    pub fn decompress(&self, data: &[u8]) -> Result<Vec<u8>, VaultError> {
        let err = |e: std::io::Error| VaultError::Compression(e.to_string());
        match &self.dict {
            Some(d) => {
                let mut dec = zstd::bulk::Decompressor::with_dictionary(d).map_err(err)?;
                dec.decompress(data, MAX_CHUNK_SIZE).map_err(err)
            }
            None => zstd::bulk::decompress(data, MAX_CHUNK_SIZE).map_err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zstd_roundtrip_no_dict() {
        let codec = Codec::new(None);
        let data = vec![42u8; 100_000];
        let packed = codec.compress(&data).unwrap();
        assert!(packed.len() < data.len());
        assert_eq!(codec.decompress(&packed).unwrap(), data);
    }

    #[test]
    fn zstd_roundtrip_with_raw_dict() {
        // «content-only» словарь (raw bytes) — zstd принимает его как raw dict
        let dict = b"the quick brown fox jumps over the lazy dog".repeat(50);
        let codec = Codec::new(Some(dict));
        let data = b"the quick brown fox".repeat(1000);
        let packed = codec.compress(&data).unwrap();
        assert_eq!(codec.decompress(&packed).unwrap(), data);
    }
}
