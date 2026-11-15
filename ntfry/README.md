# ntfry — Zero-Knowledge encrypted cloud vault

`ntfry` is a library and CLI for building an encrypted virtual file system on top of cloud storage providers. The implementation in `ntfry/src/*` keeps the VFS index in memory, splits files into chunks based on `CHUNK_SIZE` (default: 512 KB, clamped to 64..=4096 KB), compresses them with `ZSTD` (optional custom dictionary), protects them with `ChaCha20-Poly1305`, and uploads the result to cloud storage as `/ntfry/chunks/<sha256>`.

The provider only sees encrypted blocks and service files such as `/ntfry/meta`, `/ntfry/index.enc`, and `/ntfry/user.dict`; it does not see file names, directory structure, or plaintext data.

```
[ local file / app / UI ]
                │
                ▼
┌───────────────────────────────────────────────────────────────┐
│ 1. VFS / in-memory index    (path → file entry + deduplication) │
│ 2. ZSTD compression         (default or custom user.dict)      │
│ 3. ChaCha20-Poly1305 AEAD   (BIP-39 → BIP-32 master key)      │
│ 4. Storage adapter          (local / yandex / gdrive)          │
└───────────────────────────────────────────────────────────────┘
```

## Key derivation and encryption

The code derives keys strictly from a BIP-39 mnemonic:

- `m/999'/0'/0'/0` → master encryption key (`master_key`, 32 bytes)
- `m/44'/60'/0'/0/0` → ETH-compatible wallet address (`0x...`)

Keys are created using `crypto::derive()` and `crypto::derive_flexible()`, and are then used for `VaultIndex` encryption and chunk encryption.

Encryption format:

- nonce: 12 bytes
- ciphertext + tag: `ChaCha20-Poly1305`
- encrypted index is stored as `index.enc`
- `/ntfry/meta` stores a public fingerprint (`key_fingerprint`) to check whether the remote vault belongs to the same wallet without downloading the index

## Remote storage layout

```text
/ntfry/
├── index.enc         # encrypted VaultIndex (bincode -> ChaCha20-Poly1305)
├── meta              # public key fingerprint for ownership validation
├── user.dict         # optional custom ZSTD dictionary
├── chunks/
│   ├── 4a8f9c...     # encrypted chunk, filename = sha256(encrypted chunk)
│   └── ...
└── ...
```

## Index and local cache

`VaultIndex` stores:

- `files`: path → `FileEntry`
- `dirs`: explicitly created directories
- `dedup`: `sha256(plaintext chunk)` → encrypted chunk id
- `chunk_meta`: precise metadata for stats recalculation
- `stats`: raw/stored/dedup-saved bytes

The local cache is created via `sled`:

```text
./cache/<provider>/vault_index.sled
```

When `Vault::open()` runs, the index is loaded from Sled first and then validated against the remote `index.enc` using SHA-256. If the remote vault changed, the index is downloaded and the cache is refreshed. If `/ntfry` was deleted externally, the cache is reset to avoid resurrecting deleted data from stale state.

## Configuration and CLI parameters

The implementation supports the following global CLI arguments:

```bash
ntfry [--provider local|yandex|gdrive] [--mnemonic <phrase>] \
      [--local-root <path>] [--cache-dir <path>] [--dict <path>] <command>
```

- `--provider` — storage backend (`local`, `yandex`, `gdrive`)
- `--mnemonic` — BIP-39 mnemonic (12/24 words)
- `--local-root` — local mock-cloud root for the local provider
- `--cache-dir` — Sled cache folder
- `--dict` — local ZSTD dictionary overriding the remote one

There is also the environment variable `CHUNK_SIZE` (in KB), read once at process startup and clamped to `64..=4096`. The default chunk size is `512 * 1024` bytes.

## CLI

Common commands:

```bash
# 1) initialize a new vault on the local provider
ntfry --provider local init

# 2) initialize with a saved mnemonic
export NTFRY_MNEMONIC="abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
ntfry --provider yandex --mnemonic "$NTFRY_MNEMONIC" init

# 3) upload a file into the VFS
ntfry --provider local push --src ./photo.jpg --dst /photos/photo.jpg

# 4) download a file from the VFS
ntfry --provider local pull --src /photos/photo.jpg --dst ./restored.jpg

# 5) list a directory
ntfry --provider local ls --path /photos

# 6) create a directory in the virtual filesystem
ntfry --provider local mkdir --path /archive

# 7) delete a file or directory
ntfry --provider local rm --path /old.txt

# 8) train a ZSTD dictionary from a folder of files
ntfry --provider local --mnemonic "$NTFRY_MNEMONIC" \
  dict --train-from ./training-files --max-size 65536
```

### Environment variables

| Variable | Purpose |
|---|---|
| `NTFRY_PROVIDER` | `local` \| `yandex` \| `gdrive` |
| `NTFRY_MNEMONIC` | BIP-39 mnemonic (12/24 words) |
| `NTFRY_YANDEX_TOKEN` | OAuth token for Yandex Disk |
| `NTFRY_GDRIVE_TOKEN` | OAuth token for Google Drive |
| `NTFRY_LOCAL_ROOT` | root of the local mock cloud, default `./mock_cloud` |
| `NTFRY_CACHE_DIR` | Sled cache directory, default `./cache` |
| `NTFRY_DICT` | path to a local ZSTD dictionary |
| `CHUNK_SIZE` | chunk size in KB, `64..=4096`, default `512` |

## Yandex OAuth

For `--provider yandex`, you need an OAuth token for Yandex Disk. You can get it from the authorization page:

```text
https://oauth.yandex.ru/authorize?response_type=token&client_id=<CLIENT_ID>
```

After authorization, the token appears in the URL as `#access_token=...`. Then export it into your environment:

```bash
export NTFRY_YANDEX_TOKEN="y0_AgA..."
```

## Current implementation features

Based on the source code, the project currently supports:

- local storage (`LocalStorageProvider`)
- Yandex Disk (`YandexDiskProvider`)
- Google Drive (`GoogleDriveProvider`)
- training and use of a custom `user.dict`
- deduplication by SHA-256 of plaintext chunks
- precise stats recalculation for `raw_bytes`, `stored_bytes`, and `dedup_saved_bytes`
- local Sled cache and remote `index.enc` sync validation
- `meta` key fingerprint validation to detect a different wallet without opening the full vault
- GC of orphaned chunks via `delete_chunk()`

## Tests

```bash
cargo test
```

The tests cover:

- deterministic BIP-39/BIP-32 derivation
- AEAD round-trips and tamper detection
- ZSTD compression/decompression
- correct serialization and deserialization of `VaultIndex`
- transient error handling in the storage layer
