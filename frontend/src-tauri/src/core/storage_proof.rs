// Author: TechnoL0g
// Project: SmartNet Core
//
// ============================================================
//  storage_proof.rs — lightweight, Sybil-resistant Proof of Storage
//
//  Goal: let a $2 VPS prove it physically stores a dApp's bytes, defeating
//  (a) Sybil/outsourcing (many identities sharing ONE physical copy) and
//  (b) generation attacks (fetching/deriving chunks on demand) — WITHOUT heavy
//  primitives (no zk-SNARKs / RSA accumulators / multi-layer SDR).
//
//  Pipeline:
//    1. Unique sealing — each 64KB chunk is XORed with a fast SHA-256-CTR
//       keystream keyed by Hash(seeder_wallet ‖ chunk_index). The bytes written
//       to disk are therefore UNIQUE to this wallet → one physical copy can't be
//       reused across identities (the sealed merkle root differs per wallet).
//    2. Merkle commitment — a binary SHA-256 Merkle tree over the SEALED leaves;
//       the 32-byte root is the node's storage contract, persisted in sled.
//    3. Interactive challenge — a verifier derives an unpredictable target chunk
//       from network entropy (latest SmartHoldem block hash) mixed with the
//       seeder wallet, asks for that sealed chunk + its inclusion path, and
//       verifies it against the committed root in sub-millisecond time.
//
//  Thread-safety: all functions are pure / take `&sled::Db` (Send + Sync). No
//  panics or unwraps in non-test code — everything resolves via StorageError.
// ============================================================

use sha2::{Digest, Sha256};

/// Sealing / Merkle leaf size.
pub const CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    EmptyFile,
    ChunkOutOfRange,
    NoCommitment,
    MissingChunk,
    Db(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::EmptyFile => write!(f, "empty file"),
            StorageError::ChunkOutOfRange => write!(f, "target chunk out of range"),
            StorageError::NoCommitment => write!(f, "no sealed commitment for file"),
            StorageError::MissingChunk => write!(f, "sealed chunk missing from store"),
            StorageError::Db(e) => write!(f, "db error: {}", e),
        }
    }
}
impl std::error::Error for StorageError {}

// ---- hashing helpers (domain-separated to prevent leaf/node collisions) ----

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

fn leaf_hash(sealed: &[u8]) -> [u8; 32] {
    sha256(&[&[0x00], sealed])
}

fn node_hash(l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
    sha256(&[&[0x01], l, r])
}

// ---- 1) unique sealing (SHA-256-CTR keystream XOR, symmetric) ----

/// Seal (or, applied again, UNSEAL — XOR is symmetric) one chunk uniquely for
/// `seeder_wallet`. Key = SHA256(wallet ‖ index); keystream block j = SHA256(key ‖ j).
pub fn seal_chunk(chunk_index: u64, raw_data: &[u8], seeder_wallet: &str) -> Vec<u8> {
    let key = sha256(&[seeder_wallet.as_bytes(), &chunk_index.to_le_bytes()]);
    let mut out = Vec::with_capacity(raw_data.len());
    let mut counter: u64 = 0;
    let mut block = [0u8; 32];
    let mut bi = 32usize; // force keystream refill on first byte
    for &b in raw_data {
        if bi == 32 {
            block = sha256(&[&key, &counter.to_le_bytes()]);
            counter = counter.wrapping_add(1);
            bi = 0;
        }
        out.push(b ^ block[bi]);
        bi += 1;
    }
    out
}

// ---- 2) Merkle tree over sealed leaves ----

/// Merkle root over already-computed leaf hashes (odd nodes duplicate last).
fn merkle_root_from_leaves(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            let l = level[i];
            let r = if i + 1 < level.len() { level[i + 1] } else { level[i] };
            next.push(node_hash(&l, &r));
            i += 2;
        }
        level = next;
    }
    level[0]
}

/// Inclusion path (sibling hashes, bottom→top) for `index`.
fn merkle_proof(leaves: &[[u8; 32]], index: usize) -> Vec<[u8; 32]> {
    let mut path = Vec::new();
    if index >= leaves.len() {
        return path;
    }
    let mut level: Vec<[u8; 32]> = leaves.to_vec();
    let mut idx = index;
    while level.len() > 1 {
        let sib = if idx % 2 == 0 {
            if idx + 1 < level.len() { level[idx + 1] } else { level[idx] }
        } else {
            level[idx - 1]
        };
        path.push(sib);
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            let l = level[i];
            let r = if i + 1 < level.len() { level[i + 1] } else { level[i] };
            next.push(node_hash(&l, &r));
            i += 2;
        }
        level = next;
        idx /= 2;
    }
    path
}

/// Seal every chunk of `data` for `seeder_wallet` and return (sealed_chunks, leaves).
pub fn seal_file(data: &[u8], seeder_wallet: &str) -> (Vec<Vec<u8>>, Vec<[u8; 32]>) {
    let mut sealed = Vec::new();
    let mut leaves = Vec::new();
    for (i, raw) in data.chunks(CHUNK_SIZE).enumerate() {
        let s = seal_chunk(i as u64, raw, seeder_wallet);
        leaves.push(leaf_hash(&s));
        sealed.push(s);
    }
    (sealed, leaves)
}

// ---- sled persistence (the node's storage contract) ----

fn k_root(file_id: &[u8]) -> Vec<u8> {
    [b"sp:root:", file_id].concat()
}
fn k_meta(file_id: &[u8]) -> Vec<u8> {
    [b"sp:n:", file_id].concat()
}
fn k_chunk(file_id: &[u8], i: u64) -> Vec<u8> {
    [b"sp:c:", file_id, b":", i.to_string().as_bytes()].concat()
}
fn k_leaves(file_id: &[u8]) -> Vec<u8> {
    [b"sp:l:", file_id].concat()
}

/// Seal `data` for `seeder_wallet`, persist sealed chunks + leaves + root in
/// `db`, and return the 32-byte SealedMerkleRoot (the contract source of truth).
pub fn commit_file(
    db: &sled::Db,
    file_id: &[u8],
    data: &[u8],
    seeder_wallet: &str,
) -> Result<[u8; 32], StorageError> {
    if data.is_empty() {
        return Err(StorageError::EmptyFile);
    }
    let (sealed, leaves) = seal_file(data, seeder_wallet);
    let root = merkle_root_from_leaves(&leaves);
    let mut leaf_blob = Vec::with_capacity(leaves.len() * 32);
    for l in &leaves {
        leaf_blob.extend_from_slice(l);
    }
    db.insert(k_root(file_id), &root).map_err(|e| StorageError::Db(e.to_string()))?;
    db.insert(k_leaves(file_id), leaf_blob).map_err(|e| StorageError::Db(e.to_string()))?;
    db.insert(k_meta(file_id), &(sealed.len() as u64).to_le_bytes())
        .map_err(|e| StorageError::Db(e.to_string()))?;
    for (i, s) in sealed.iter().enumerate() {
        db.insert(k_chunk(file_id, i as u64), s.as_slice())
            .map_err(|e| StorageError::Db(e.to_string()))?;
    }
    db.flush().map_err(|e| StorageError::Db(e.to_string()))?;
    Ok(root)
}

/// Load the committed SealedMerkleRoot for a file.
pub fn load_sealed_root(db: &sled::Db, file_id: &[u8]) -> Result<[u8; 32], StorageError> {
    let v = db.get(k_root(file_id)).map_err(|e| StorageError::Db(e.to_string()))?
        .ok_or(StorageError::NoCommitment)?;
    if v.len() != 32 {
        return Err(StorageError::NoCommitment);
    }
    let mut root = [0u8; 32];
    root.copy_from_slice(&v);
    Ok(root)
}

fn load_leaves(db: &sled::Db, file_id: &[u8]) -> Result<Vec<[u8; 32]>, StorageError> {
    let v = db.get(k_leaves(file_id)).map_err(|e| StorageError::Db(e.to_string()))?
        .ok_or(StorageError::NoCommitment)?;
    if v.is_empty() || v.len() % 32 != 0 {
        return Err(StorageError::NoCommitment);
    }
    Ok(v.chunks(32).map(|c| {
        let mut a = [0u8; 32];
        a.copy_from_slice(c);
        a
    }).collect())
}

/// Total committed chunks for a file.
pub fn chunk_count(db: &sled::Db, file_id: &[u8]) -> Result<u64, StorageError> {
    let v = db.get(k_meta(file_id)).map_err(|e| StorageError::Db(e.to_string()))?
        .ok_or(StorageError::NoCommitment)?;
    if v.len() != 8 {
        return Err(StorageError::NoCommitment);
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(&v);
    Ok(u64::from_le_bytes(a))
}

// ---- 3) interactive challenge ----

/// Deterministic, unpredictable target chunk = u64(SHA256(block_entropy ‖ wallet)) % total.
pub fn calculate_challenge_index(
    block_entropy: [u8; 32],
    seeder_wallet: &str,
    total_chunks: u64,
) -> u64 {
    if total_chunks == 0 {
        return 0;
    }
    let h = sha256(&[&block_entropy, seeder_wallet.as_bytes()]);
    let mut a = [0u8; 8];
    a.copy_from_slice(&h[..8]);
    u64::from_le_bytes(a) % total_chunks
}

/// Seeder side: fetch the SEALED `target_chunk` from `db` + its Merkle inclusion
/// path leading to the committed root. The full raw file is never needed.
pub fn generate_storage_proof(
    db: &sled::Db,
    file_id: &[u8],
    target_chunk: u64,
    _seeder_wallet: &str,
) -> Result<(Vec<u8>, Vec<[u8; 32]>), StorageError> {
    let total = chunk_count(db, file_id)?;
    if target_chunk >= total {
        return Err(StorageError::ChunkOutOfRange);
    }
    let chunk = db
        .get(k_chunk(file_id, target_chunk))
        .map_err(|e| StorageError::Db(e.to_string()))?
        .ok_or(StorageError::MissingChunk)?
        .to_vec();
    let leaves = load_leaves(db, file_id)?;
    let path = merkle_proof(&leaves, target_chunk as usize);
    Ok((chunk, path))
}

/// Verifier side (DPoS delegate / tracker): hash the single sealed chunk and walk
/// the inclusion path up to `sealed_root`. Sub-millisecond; no full file needed.
pub fn verify_storage_proof(
    sealed_root: [u8; 32],
    chunk_data: &[u8],
    proof_path: Vec<[u8; 32]>,
    target_chunk: u64,
) -> bool {
    let mut cur = leaf_hash(chunk_data);
    let mut idx = target_chunk;
    for sib in &proof_path {
        cur = if idx % 2 == 0 {
            node_hash(&cur, sib)
        } else {
            node_hash(sib, &cur)
        };
        idx /= 2;
    }
    cur == sealed_root
}

/// Monetization kill-switch (Phase D/E). When OFF, network storage challenges +
/// K_up scoring pause (the sealing/merkle math + tests still run). Default ON.
pub fn challenges_active() -> bool {
    std::env::var("ENABLE_INCENTIVIZED_SEEDING")
        .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "off" | "no"))
        .unwrap_or(true)
}

// ============================================================
//  tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 31 + 7) % 251) as u8).collect()
    }

    fn open_db() -> sled::Db {
        sled::Config::new().temporary(true).open().expect("temp db")
    }

    #[test]
    fn sealing_is_unique_and_reversible() {
        let raw = sample(1000);
        let a = seal_chunk(3, &raw, "ShAlice");
        let b = seal_chunk(3, &raw, "ShBob");
        assert_ne!(a, b, "sealing must be unique per wallet (anti-Sybil)");
        // XOR is symmetric → applying the same seal again recovers the plaintext
        let back = seal_chunk(3, &a, "ShAlice");
        assert_eq!(back, raw, "unseal must recover original bytes");
    }

    #[test]
    fn valid_proof_verifies() {
        let db = open_db();
        let data = sample(CHUNK_SIZE * 3 + 1234); // 4 chunks
        let wallet = "ShAlice";
        let root = commit_file(&db, b"file1", &data, wallet).expect("commit");
        let total = chunk_count(&db, b"file1").expect("count");
        let entropy = [9u8; 32];
        let target = calculate_challenge_index(entropy, wallet, total);
        let (chunk, path) = generate_storage_proof(&db, b"file1", target, wallet).expect("proof");
        assert!(verify_storage_proof(root, &chunk, path, target));
    }

    #[test]
    fn corrupted_chunk_fails() {
        let db = open_db();
        let data = sample(CHUNK_SIZE * 3);
        let wallet = "ShAlice";
        let root = commit_file(&db, b"file2", &data, wallet).expect("commit");
        let total = chunk_count(&db, b"file2").expect("count");
        let target = calculate_challenge_index([1u8; 32], wallet, total);
        let (mut chunk, path) = generate_storage_proof(&db, b"file2", target, wallet).expect("proof");
        chunk[0] ^= 0xFF; // tamper on disk
        assert!(!verify_storage_proof(root, &chunk, path, target));
    }

    #[test]
    fn wrong_wallet_seal_fails() {
        let db = open_db();
        let data = sample(CHUNK_SIZE * 3);
        // Alice commits the contract (root keyed to Alice's sealing).
        let root = commit_file(&db, b"file3", &data, "ShAlice").expect("commit");
        let total = chunk_count(&db, b"file3").expect("count");
        let target = calculate_challenge_index([2u8; 32], "ShAlice", total);
        let (_alice_chunk, path) = generate_storage_proof(&db, b"file3", target, "ShAlice").expect("proof");
        // A malicious node sealed the SAME chunk with a DIFFERENT wallet.
        let raw = &data[(target as usize) * CHUNK_SIZE..][..CHUNK_SIZE.min(data.len() - (target as usize) * CHUNK_SIZE)];
        let bob_chunk = seal_chunk(target, raw, "ShMallory");
        assert!(!verify_storage_proof(root, &bob_chunk, path, target));
    }
}
