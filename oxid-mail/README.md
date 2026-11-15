# Oxid Mail - monorepo

A secure email client for the NETFORY ecosystem. Identity is a BIP-39 seed (SmartHoldem),
and all data on disk is encrypted (XChaCha20-Poly1305) and compressed (zstd).

## Structure

```
oxid-mail/
├── ui/                    # Vue 3 + Vite + Pinia + Tailwind (see ui/README.md for design-system docs)
├── crates/
│   ├── mail-core/         # common core: BIP-39 identity, IMAP (rustls), SMTP (lettre), email parser
│   ├── mail-store/        # encrypted + compressed storage: sled + zstd + XChaCha20-Poly1305 (key derived from seed via HKDF)
│   └── oxid-bridge/       # dev/web bridge (axum, :8002) - same command surface as the future Tauri backend
└── src-tauri/             # (stage 2) standalone Tauri build
```

## Two operating modes (by design)

1. **Web-dev (current)**: UI on :3001 calls HTTP endpoints in `oxid-bridge` on :8002.
2. **Tauri (stage 2)**: the same UI calls `invoke()` - commands match the bridge routes.
   The core (`mail-core`, `mail-store`) is transport-agnostic, so the client can be built
   both as a standalone app and as a child window of the NETFORY client.

## Running in development

```bash
# 1. Bridge (Rust)
cd oxid-mail
cargo run -p oxid-bridge          # listens on 0.0.0.0:8002 (env OXID_BRIDGE_PORT)

# 2. UI
cd oxid-mail/ui
yarn && yarn dev                  # http://localhost:3001
```

## Native app build (Tauri) - in one command

```bash
cd oxid-mail/ui
yarn                # one-time: installs dependencies, including @tauri-apps/cli
yarn app:build      # (= cd .. && tauri build) builds the UI and native binary + installers
```

Result: `oxid-mail/target/release/oxid-mail(.exe)` (binary; target is at the workspace root)
and `oxid-mail/target/release/bundle/*` (deb/rpm/AppImage/msi/dmg depending on platform).

> The script does `cd ..` and launches Tauri from the monorepo root - the CLI looks for
> `src-tauri/tauri.conf.json` in subfolders of the current directory.

Native window dev mode with hot reload: `yarn app:dev`.

Linux requirements: `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev pkg-config build-essential`.
On Windows: WebView2 (usually already installed) + Rust toolchain. On macOS: Xcode CLT.

Starting from NETFORY: place the built binary `oxid-mail` next to the NETFORY binary
(or in a `./oxid-mail/` subfolder, or set the path in `OXID_MAIL_BIN`) - the
"✉ MAIL" button in the sidebar and the Mail card on the dashboard launch it as a separate process.

## Communication with NETFORY (IPC: stdin/stdout, JSONL)

NETFORY starts Oxid Mail as a **child process with pipes** - this is both a secure secret handoff and a persistent bidirectional channel:

- **NETFORY -> mail (child stdin)**, one JSON string per message:
  - `{"type":"handshake","v":1,"mnemonic":"..."}` - the current NETFORY session seed.
    Sent immediately after startup. **The seed exists only in RAM of the two processes**:
    it never enters argv/env/disk and is not visible in the task manager. The parent↔child pipe cannot be intercepted externally. No PIN is needed in the mail app in this mode - authorization has already been validated during NETFORY unlock, and the mail UI opens a session directly without onboarding. In this mode (`ephemeral`), mail does not write to localStorage - if launched standalone without NETFORY, the user must enter the seed or PIN as usual.
  - `{"type":"focus"}` - clicking "Mail" again does not spawn a new process,
    but focuses the already-open window.
- **Mail -> NETFORY (child stdout)**:
  - `{"type":"request_seed"}` - pull request: mail asks for the seed
    (auto-login at startup and the **Connect NETFORY** button on the onboarding screen -
    if auto-login fails for any reason, the user clicks it and signs in without manually entering 12 words);
  - `{"type":"handshake_ack"}` - seed receipt confirmation;

> NETFORY wallet phrases may be **arbitrary** (not necessarily BIP-39):
> BIP-39 validation is skipped during handoff (`identity::from_secret`, derivation is the same as NETFORY: `privkey = sha256(secret.trim())`, case matters).
> BIP-39 validation remains only for manual entry of 12 words during onboarding.
  - `{"type":"new_mail","count":N,"from":"...","subject":"..."}` - new email; NETFORY shows a toast (click to open/focus mail).

Why not sockets (Named Pipes / Unix sockets): the parent↔child pipe requires no authentication of the connecting process and adds no extra dependencies. If external connection to an already-running client is needed later, a socket can be added then, and a one-time token for it can be passed through the same stdin.

## New mail notifications

When IMAP IDLE detects a new message (or it is found by periodic polling - every 20s, also while minimized), a **system notification** is shown (tauri-plugin-notification), and a `new_mail` event is sent to NETFORY over the IPC channel.

## Combined build: one MSI = NETFORY + Oxid Mail

From the `frontend` folder:

```bash
yarn tauri:build:bonsai:mail
```

The script: 1) builds Oxid Mail (`node scripts/build-oxid-mail.cjs` -> release binary),
2) copies `oxid-mail.exe` into `frontend/src-tauri/`,
3) builds the NETFORY MSI with the merge config `src-tauri/tauri.mail.conf.json`
(`bundle.resources: ["oxid-mail.exe"]`) - the installer places `oxid-mail.exe`
next to the NETFORY exe in the install folder, where `launch_oxid_mail` finds it.

Data is stored in `./oxid-data/{STH-address}/` (env `OXID_DATA_DIR`; in Tauri, the default is the system appData folder), and the contents are unreadable without the seed phrase.

## Gmail: app-password sign-in (OAuth button hidden)

Since 2026-06, the **Sign in with Google - 1 click** button is hidden by user decision:
the only supported path for Gmail is **app password** (App Password) according to the instructions
at myaccount.google.com/apppasswords. The OAuth core (XOAUTH2, refresh_token)
remains in the code (`oauth.rs`, command `oauth_gmail_sign_in`) and can be restored with a single button in the UI.

## Server-side flags 

Read state (`\Seen`) and stars (`\Flagged`) are pushed to the IMAP server when changed;
delete from trash deletes the email on the server as well (`+\Deleted`, EXPUNGE).
Moving to trash is still local-only. Auto-check for new mail runs every 2 minutes,
and the unread counter is displayed in the window title.

## Security and encryption of on-disk data

**How mail is encrypted (sled storage, crate `mail-store`):**

- Storage key: `HKDF-SHA256(sha256(mnemonic), salt "oxid-mail/store/v1")` -
  derived from the seed phrase; the key itself is never written to disk.
- Every value in sled (email bodies, JSON account data with passwords, metadata):
  `zstd compression -> XChaCha20-Poly1305` with a random 24-byte nonce.
  There is never an unencrypted byte of content on disk.
- **Attachments** are stored the same way - as separate encrypted blobs
  (`put_blob`, tree `atts:{account}`), and are only decrypted when downloaded.
- Opening an existing store with a different seed is impossible (the `__check__` marker
  is validated on open; AEAD authentication rejects the wrong key).
- Data lives in `appData/oxid-data/{STH-address}` - each identity has its own directory.

**How the seed itself is protected:**

- Launch from NETFORY: the seed is passed through stdin IPC and lives **only in RAM** -
  it is never written to disk, no PIN is needed (authorization is already verified in NETFORY).
- Standalone launch: the seed can be sealed with a PIN (`pin_seal`) - on disk only a blob is stored,
  encrypted with the same XChaCha20-Poly1305 using a key derived from the PIN;
  without the PIN, the plaintext seed remains unavailable.
- Private key: `sha256(mnemonic)` -> secp256k1; address `S…` = base58check(0x3F ‖ ripemd160(pubkey)) -
  identical to NETFORY/SmartHoldem client behavior.
- Mail account passwords are stored only inside the encrypted store and never leave the device.

## Providers (stage 1)

Gmail / Yandex / Yahoo - via **app password**, Custom IMAP - manual setup.
IMAP login is verified when the account is added. OAuth for Gmail is stage 2.

## What syncs (stage 1)

- INBOX (last 50 emails) + "Sent" folder (30) - pull-only.
- Read state/stars/trash - local-only state (not pushed to the server, stage 2).
- Sending - SMTP (465 = TLS, otherwise STARTTLS), a copy is saved locally in Sent.

## Tests

```bash
cargo test --workspace   # identity (BIP-39/addresses), mail-store (encryption/compression), RFC-822 parser
```
