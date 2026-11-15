# SmartNet — SmartHoldem Decentralized P2P App Client

Vue 3 + Vite frontend with a Tauri v2 (Rust + Sled) native shell and a full
Web Fallback (Pinia + localStorage/sessionStorage). Tactical-minimalist
cyberpunk UI, 3 themes, RU/EN, real SmartHoldem crypto.

## Run — Web (browser)
```bash
yarn install
yarn dev          # http://localhost:3000
```

## Run / Build — Tauri (desktop, local)
```bash
yarn tauri:dev    # native dev window
yarn tauri:build  # production bundle
yarn tauri:icons  # regenerate icons from src-tauri/icons/icon.png (1024x1024)
```
> Requires the Rust toolchain + Tauri v2 prerequisites installed locally.

## Crypto
- BIP-39 mnemonic (12 words) → BIP-44 HD at `m/44'/111'/N'/0/0` on secp256k1.
- Address: `base58check( 0x3F ‖ RIPEMD160(compressedPubKey) )` → `S...`.
- Mnemonic AES-sealed with `SHA-256(PIN)`; raw PIN never persisted.

## Environment detection
```ts
const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
```
- Tauri → Rust IPC (Sled DB), decrypted seed kept in Rust process memory.
- Web → localStorage (`sth_vault`) + sessionStorage (`sth_session_pin`, survives F5).

## Layout
`src/lib/crypto.ts` · `src/lib/bridge.ts` · `src/store/{auth,ui}.ts` ·
`src/layouts/AppLayout.vue` · `src/views/*` · `src/i18n/*` · `src-tauri/*`

## dApp tabs — native child webviews vs iframes (feasibility)

**Question:** can `sth://` apps render in *real Tauri child webviews* embedded
under the tab UI (instead of iframes used by the web fallback)?

**Answer: yes, via Tauri v2's multi-webview API**, with caveats:

- It requires the **`unstable`** Cargo feature (`tauri = { features = ["unstable"] }`).
  This API (`Window::add_child(WebviewBuilder, position, size)`) is **not part of
  the stable surface** and can change between minor Tauri releases.
- A child webview is a real OS webview attached to the main window — **not a DOM
  element**. So it always paints **on top** of the Vue layer at absolute OS
  coordinates. The frontend must therefore:
  1. measure the host `<div>`'s `getBoundingClientRect()` and push those bounds
     to the native side (`open_embedded_webview` / `set_embedded_bounds`),
  2. re-sync on window resize, sidebar collapse, and tab switch,
  3. `hide` inactive tabs and `close` removed tabs, `close` all on LOCK.
- Because it floats above the DOM, overlays/menus from the Vue shell will be
  hidden behind the webview unless explicitly accounted for. Modals should be
  rendered before opening, or the embedded webview hidden while a modal is up.

**Implementation here:**
- Rust commands in `src-tauri/src/lib.rs`: `open_embedded_webview`,
  `set_embedded_bounds`, `hide_embedded_webview`, `close_embedded_webview`
  (labels `embed-<sanitized-address>`), loading `sth://<id>/` via the custom
  protocol — zero local HTTP server.
- `AppLayout.vue` drives the lifecycle through a `ResizeObserver` + watchers on
  the active tab; the web fallback keeps the `<iframe>` placeholder.
- Capabilities updated in `capabilities/default.json`
  (`webviews: ["main","embed-*"]` + webview position/size/show/hide/close perms).

## content.json verification (ZeroNet trust)
On open, `sth://<id>` is verified before it is trusted:
1. **files re-hashed on disk** (SHA-256) → recomputed Blake3 merkle must match
   the manifest (native `verify_app`);
2. `id` must equal the SmartHoldem address derived from the signing **publicKey**
   (the URL *is* the key);
3. the ECDSA signature over `sha256(id|name|merkle)` must be valid.
A `✓ verified` / `⚠ unverified` badge is shown on the tab. Web fallback verifies
the stored manifest signature (`lib/crypto.ts → verifyAppManifest`).

## P2P LAN discovery (mDNS) + iroh-blobs transit
`start_discovery` / `discovered_peers` / `stop_discovery` register this node on
`_smartnet._udp.local.` (announcing `node`, iroh `nodeId`, and `app_id:blobHash`
pairs in TXT) and browse the LAN for other SmartNet nodes.

- **Discover Apps page** (`views/Discovered.vue`, sidebar `DISCOVER APPS`):
  aggregates peer apps **deduplicated by id**, showing title, description
  (fallback `no title` / `no description`) and a **seeds** count = number of
  distinct peers serving that app. An **Install** button is also in
  Settings → P2P DISCOVERY.
- **Real transit (`iroh-blobs`)** is **OPTIONAL, behind the Cargo `p2p` feature**
  (kept off by default so the desktop build always compiles while iroh's API
  stabilises). Build with `cargo build --features p2p` (or add `"p2p"` to
  `[features].default` in `src-tauri/Cargo.toml`). On discovery each local dApp
  folder is seeded into an `FsStore` → its collection hash announced; `install_app`
  downloads the collection from a serving node and exports it to
  `data/dapps/<id>`, then `verify_app` proves integrity → `✓` badge.
  ⚠️ Pin a mutually compatible `iroh` / `iroh-blobs` pair from crates.io and adjust
  the API in the `p2p_impl` module (in `lib.rs`) if it drifted.

## Publishing requirements
A title (name) **and** a short description are **mandatory** — `publish_app`
(native) and the web store both reject empty values. The description is part of
the signed digest (`sha256(id|name|description|merkle)`), so it is tamper-evident.

## z-order (native child webviews vs Vue modals)
Native child webviews paint above the DOM, so the `ui` store tracks an
`overlayOpen` flag (incremented by modals, e.g. the QR backup). While an overlay
is open, `AppLayout` hides the active embedded webview and restores it on close.


