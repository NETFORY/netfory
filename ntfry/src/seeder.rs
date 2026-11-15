//! Cloud Seeder (ntfryseed) — публичный анти-цензурный слой поверх облаков.
//! В отличие от приватного сейфа (E2EE от всех), цель сидера —
//! Аутентификация + Обфускация: облако видит только шифромусор, а любой
//! клиент SmartNet, знающий публичный ключ автора, детерминированно
//! выводит ключ расшифровки (HKDF-SHA256, соль SmartNet_Cloud_Seeder)
//! и проверяет ECDSA-подпись автора после расшифровки.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
use secp256k1::ecdsa::Signature;
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{CryptoError, StorageError};

/// Статическая соль деривации публичного симметричного ключа.
pub const SEEDER_SALT: &[u8] = b"SmartNet_Cloud_Seeder";
/// Имя общедоступной папки сидера на облачном диске.
pub const SEED_DIR: &str = "ntfryseed";
/// Файлы состояния сети внутри /ntfryseed.
pub const SEED_FILES: [&str; 4] = ["dapps.enc", "nodes.enc", "seeders.enc", "vdapps.enc"];
/// Подпапка с зашифрованными tar-архивами dApp внутри /ntfryseed.
pub const SEED_DAPPS_DIR: &str = "dapps";
/// Подпапка каталожных медиа (logo/preview) — облачный fallback discovery_cache.
pub const SEED_MEDIA_DIR: &str = "discovery_cache";

// ── Детерминированное публичное шифрование ────────────────────────────────

/// PubKey автора → 32-байтный симметричный ключ (HKDF-SHA256).
/// Любой клиент с тем же pubkey получает тот же ключ — паролей не нужно.
pub fn derive_seeder_key(pubkey: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(SEEDER_SALT), pubkey);
    let mut okm = [0u8; 32];
    hk.expand(b"ntfryseed-v1", &mut okm).expect("HKDF: 32 байта OKM валидны");
    okm
}

/// Конверт: подписанный payload внутри шифроблока.
#[derive(Serialize, Deserialize)]
struct Envelope {
    v: u8,
    /// compressed secp256k1 pubkey автора, hex (66 символов)
    pubkey: String,
    /// компактная ECDSA-подпись sha256(payload), hex (128 символов)
    sig: String,
    /// исходный JSON-текст состояния сети
    payload: String,
}

/// Конвейер публикации: подпись приватным ключом кошелька → конверт →
/// ChaCha20-Poly1305 ключом, выведенным из ПУБЛИЧНОГО ключа автора.
pub fn seal(payload: &str, sign_priv: &[u8; 32]) -> Result<Vec<u8>, CryptoError> {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(sign_priv)
        .map_err(|e| CryptoError::Derivation(e.to_string()))?;
    let pk = PublicKey::from_secret_key(&secp, &sk);
    let digest = Sha256::digest(payload.as_bytes());
    let msg = Message::from_digest_slice(&digest)
        .map_err(|e| CryptoError::Malformed(e.to_string()))?;
    let sig = secp.sign_ecdsa(&msg, &sk);
    let env = Envelope {
        v: 1,
        pubkey: hex::encode(pk.serialize()),
        sig: hex::encode(sig.serialize_compact()),
        payload: payload.to_string(),
    };
    let plain = serde_json::to_vec(&env).map_err(|e| CryptoError::Malformed(e.to_string()))?;
    let key = derive_seeder_key(&pk.serialize());
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
        .map_err(|_| CryptoError::Aead)?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Конвейер чтения: pubkey → тот же ключ → расшифровка → проверка подписи.
/// Подмена байтов облаком или чужой pubkey → ошибка, данные отбрасываются.
pub fn open(blob: &[u8], pubkey_hex: &str) -> Result<String, CryptoError> {
    let pub_bytes = hex::decode(pubkey_hex.trim())
        .map_err(|e| CryptoError::Malformed(format!("pubkey hex: {e}")))?;
    let pk = PublicKey::from_slice(&pub_bytes)
        .map_err(|e| CryptoError::Malformed(format!("pubkey: {e}")))?;
    if blob.len() < 12 + 16 {
        return Err(CryptoError::Malformed("блок короче nonce+tag".into()));
    }
    let key = derive_seeder_key(&pub_bytes);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let plain = cipher
        .decrypt(Nonce::from_slice(&blob[..12]), &blob[12..])
        .map_err(|_| CryptoError::Aead)?;
    let env: Envelope =
        serde_json::from_slice(&plain).map_err(|e| CryptoError::Malformed(e.to_string()))?;
    if !env.pubkey.eq_ignore_ascii_case(pubkey_hex.trim()) {
        return Err(CryptoError::Malformed("pubkey конверта не совпадает с ключом сидера".into()));
    }
    let sig = Signature::from_compact(
        &hex::decode(&env.sig).map_err(|e| CryptoError::Malformed(format!("sig hex: {e}")))?,
    )
    .map_err(|e| CryptoError::Malformed(format!("sig: {e}")))?;
    let digest = Sha256::digest(env.payload.as_bytes());
    let msg = Message::from_digest_slice(&digest)
        .map_err(|e| CryptoError::Malformed(e.to_string()))?;
    Secp256k1::verification_only()
        .verify_ecdsa(&msg, &sig, &pk)
        .map_err(|_| CryptoError::Malformed("подпись автора НЕ совпадает — файл подменён".into()))?;
    Ok(env.payload)
}

// ── Бинарный конверт v2/v3: большие payload'ы (tar-архивы dApp) ───────────
//    v2: plain = [2u8] ‖ pubkey(33) ‖ sig(64) ‖ payload
//    v3: plain = [3u8] ‖ pubkey(33) ‖ sig(64) ‖ zstd(payload) — подпись над
//        СЖАТЫМИ байтами: проверяется ДО распаковки (защита от zstd-бомб).

/// Как `seal`, но для произвольных байтов (tar-архив dApp и т.п.).
/// Пишет конверт v3: payload сжимается zstd (level 3) перед шифрованием —
/// для текстовых dApp это 3–5× экономии облачной квоты.
///
/// Nonce ДЕТЕРМИНИРОВАННЫЙ (SIV-стиль): sha256(ключ ‖ сжатый payload)[..12].
/// Одинаковое содержимое → байт-в-байт одинаковый шифроблок, что позволяет
/// пропускать заливку по sha256, если архив уже лежит в облаке. Повтор nonce
/// возможен ТОЛЬКО с тем же plaintext — для AEAD это безопасно (утекает лишь
/// факт «содержимое не изменилось», а слой и так публично читаем по pubkey).
/// Подпись ECDSA детерминирована (RFC 6979), zstd детерминирован.
pub fn seal_bytes(payload: &[u8], sign_priv: &[u8; 32]) -> Result<Vec<u8>, CryptoError> {
    let packed = zstd::stream::encode_all(std::io::Cursor::new(payload), 3)
        .map_err(|e| CryptoError::Malformed(format!("zstd compress: {e}")))?;
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(sign_priv)
        .map_err(|e| CryptoError::Derivation(e.to_string()))?;
    let pk = PublicKey::from_secret_key(&secp, &sk);
    let digest = Sha256::digest(&packed);
    let msg = Message::from_digest_slice(&digest)
        .map_err(|e| CryptoError::Malformed(e.to_string()))?;
    let sig = secp.sign_ecdsa(&msg, &sk);
    let mut plain = Vec::with_capacity(1 + 33 + 64 + packed.len());
    plain.push(3u8);
    plain.extend_from_slice(&pk.serialize());
    plain.extend_from_slice(&sig.serialize_compact());
    plain.extend_from_slice(&packed);
    let key = derive_seeder_key(&pk.serialize());
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let mut h = Sha256::new();
    h.update(b"ntfryseed-nonce-v1");
    h.update(key);
    h.update(&packed);
    let nonce_full = h.finalize();
    let nonce: [u8; 12] = nonce_full[..12].try_into().expect("sha256 >= 12 байт");
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
        .map_err(|_| CryptoError::Aead)?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Как `open`, но для бинарного конверта v2/v3. Проверяет привязку pubkey и
/// ECDSA-подпись автора; v3 распаковывается zstd ПОСЛЕ проверки подписи.
pub fn open_bytes(blob: &[u8], pubkey_hex: &str) -> Result<Vec<u8>, CryptoError> {
    let pub_bytes = hex::decode(pubkey_hex.trim())
        .map_err(|e| CryptoError::Malformed(format!("pubkey hex: {e}")))?;
    let pk = PublicKey::from_slice(&pub_bytes)
        .map_err(|e| CryptoError::Malformed(format!("pubkey: {e}")))?;
    if blob.len() < 12 + 16 {
        return Err(CryptoError::Malformed("блок короче nonce+tag".into()));
    }
    let key = derive_seeder_key(&pub_bytes);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let plain = cipher
        .decrypt(Nonce::from_slice(&blob[..12]), &blob[12..])
        .map_err(|_| CryptoError::Aead)?;
    let ver = plain.first().copied().unwrap_or(0);
    if plain.len() < 1 + 33 + 64 || (ver != 2 && ver != 3) {
        return Err(CryptoError::Malformed("не бинарный конверт v2/v3".into()));
    }
    if plain[1..34] != pub_bytes[..] {
        return Err(CryptoError::Malformed("pubkey конверта не совпадает с ключом сидера".into()));
    }
    let sig = Signature::from_compact(&plain[34..98])
        .map_err(|e| CryptoError::Malformed(format!("sig: {e}")))?;
    let payload = &plain[98..];
    let digest = Sha256::digest(payload);
    let msg = Message::from_digest_slice(&digest)
        .map_err(|e| CryptoError::Malformed(e.to_string()))?;
    Secp256k1::verification_only()
        .verify_ecdsa(&msg, &sig, &pk)
        .map_err(|_| CryptoError::Malformed("подпись автора НЕ совпадает — файл подменён".into()))?;
    if ver == 3 {
        zstd::stream::decode_all(payload)
            .map_err(|e| CryptoError::Malformed(format!("zstd decompress: {e}")))
    } else {
        Ok(payload.to_vec())
    }
}

// ── URI сидера: sn://seed/<p>@<folder_token>@<public_key> ─────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeederProvider {
    Yandex,
    Google,
    Mailru,
}

impl SeederProvider {
    pub fn code(&self) -> char {
        match self {
            SeederProvider::Yandex => 'y',
            SeederProvider::Google => 'g',
            SeederProvider::Mailru => 'm',
        }
    }
}

#[derive(Debug, Clone)]
pub struct CloudSeederUri {
    pub provider: SeederProvider,
    pub folder_token: String,
    pub public_key: String,
}

impl CloudSeederUri {
    /// Парсинг `sn://seed/y@<folder_token>@<public_key>`. folder_token может
    /// содержать base64-символы (`/ + =`), поэтому pubkey отделяется СПРАВА.
    pub fn parse(uri: &str) -> Result<Self, CryptoError> {
        let rest = uri
            .trim()
            .strip_prefix("sn://seed/")
            .ok_or_else(|| CryptoError::Malformed("URI должен начинаться с sn://seed/".into()))?;
        let (prov, rest) = rest
            .split_once('@')
            .ok_or_else(|| CryptoError::Malformed("нет провайдера в URI".into()))?;
        let provider = match prov {
            "y" => SeederProvider::Yandex,
            "g" => SeederProvider::Google,
            "m" => SeederProvider::Mailru,
            other => {
                return Err(CryptoError::Malformed(format!("неизвестный провайдер '{other}'")))
            }
        };
        let (folder_token, public_key) = rest
            .rsplit_once('@')
            .ok_or_else(|| CryptoError::Malformed("нет публичного ключа в URI".into()))?;
        if folder_token.is_empty() {
            return Err(CryptoError::Malformed("пустой folder_token".into()));
        }
        let pk_bytes = hex::decode(public_key)
            .map_err(|e| CryptoError::Malformed(format!("pubkey hex: {e}")))?;
        PublicKey::from_slice(&pk_bytes)
            .map_err(|e| CryptoError::Malformed(format!("pubkey: {e}")))?;
        Ok(Self {
            provider,
            folder_token: folder_token.to_string(),
            public_key: public_key.to_lowercase(),
        })
    }

    pub fn to_uri(&self) -> String {
        format!("sn://seed/{}@{}@{}", self.provider.code(), self.folder_token, self.public_key)
    }
}

// ── Yandex Disk: выгрузка автором + анонимное чтение пирами ───────────────

const YA_API: &str = "https://cloud-api.yandex.net/v1/disk/resources";
const YA_PUBLIC: &str = "https://cloud-api.yandex.net/v1/disk/public/resources";
const YA_WEBDAV: &str = "https://webdav.yandex.ru";

/// Таймаут коротких управляющих запросов (MKCOL, publish, meta, href, DELETE).
const CTL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn http_client() -> reqwest::Client {
    // Клиент для КОРОТКИХ операций и скачиваний. read_timeout=300s защищает
    // от «сервер молчит» при GET; для длинных PUT НЕ подходит — см. upload_client.
    // User-Agent обязателен: o2.mail.ru отвечает "missing user-agent header".
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) SmartNet-ntfry/1.0")
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(300))
        .tcp_keepalive(std::time::Duration::from_secs(20))
        .build()
        .expect("reqwest client")
}

/// Клиент для ДЛИННЫХ PUT — БЕЗ read_timeout.
/// Урок v1.90.4: read_timeout у reqwest меряет время «от сервера ни байта»,
/// а при PUT сервер молчит ВСЮ загрузку. На 3G (113 КБ/с) 40 МБ льются
/// дольше 300с — reqwest рвал ЖИВОЕ соединение ровно на 300-й секунде
/// (33.8 МБ из 40.8, как по линейке), и каждая попытка упиралась в тот же
/// потолок. Стойкость даёт сторожок прогресса в put_with_stall_guard.
fn upload_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) SmartNet-ntfry/1.0")
        .connect_timeout(std::time::Duration::from_secs(15))
        .tcp_keepalive(std::time::Duration::from_secs(20))
        .build()
        .expect("reqwest client")
}

/// Стрим встал: сокет не забирает чанки дольше 90с → обрыв попытки (ретрай).
const UPLOAD_STALL: std::time::Duration = std::time::Duration::from_secs(90);
/// Все байты ушли: серверу даётся до 300с на финализацию (checksum/репликация).
const FINALIZE_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

type ProgressCb = std::sync::Arc<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>;

/// PUT со сторожком ПРОГРЕССА вместо read_timeout: пока байты реально уходят
/// в сокет — попытка живёт сколько угодно (EDGE/3G без потолка скорости);
/// прогресс встал на 90с → обрыв; после последнего байта серверу даётся
/// 300с на ответ. Прогресс меряется по выдаче 256 КиБ-чанков стрима тела.
async fn put_with_stall_guard(
    req: reqwest::RequestBuilder,
    buf: bytes::Bytes,
    attempt: u32,
    cb: Option<ProgressCb>,
) -> Result<reqwest::Response, StorageError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    const CHUNK: usize = 256 * 1024;
    let total = buf.len() as u64;
    let started = std::time::Instant::now();
    let sent = std::sync::Arc::new(AtomicU64::new(0));
    let last_ms = std::sync::Arc::new(AtomicU64::new(0));
    let len = buf.len();
    let chunks: Vec<bytes::Bytes> =
        (0..len).step_by(CHUNK).map(|i| buf.slice(i..(i + CHUNK).min(len))).collect();
    let (sent2, last2) = (sent.clone(), last_ms.clone());
    let stream = futures::stream::iter(chunks.into_iter().map(move |c| {
        let s = sent2.fetch_add(c.len() as u64, Ordering::Relaxed) + c.len() as u64;
        last2.store(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        if let Some(cb) = &cb {
            cb(s, attempt);
        }
        Ok::<_, std::io::Error>(c)
    }));
    let send = req
        .header(reqwest::header::CONTENT_LENGTH, total)
        .body(reqwest::Body::wrap_stream(stream))
        .send();
    tokio::pin!(send);
    loop {
        tokio::select! {
            r = &mut send => return r.map_err(StorageError::from),
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                let idle_ms = (started.elapsed().as_millis() as u64)
                    .saturating_sub(last_ms.load(Ordering::Relaxed));
                let idle = std::time::Duration::from_millis(idle_ms);
                let done = sent.load(Ordering::Relaxed) >= total;
                if !done && idle >= UPLOAD_STALL {
                    return Err(StorageError::Stalled(format!(
                        "стрим встал: ушло {} из {total} байт, прогресса нет {}с",
                        sent.load(Ordering::Relaxed),
                        idle.as_secs()
                    )));
                }
                if done && idle >= FINALIZE_WAIT {
                    return Err(StorageError::Stalled(format!(
                        "все {total} байт переданы, но сервер молчит {}с",
                        idle.as_secs()
                    )));
                }
            }
        }
    }
}

/// До 3 попыток PUT с бэкоффом 4/6с; прогресс новой попытки стартует с нуля.
/// Верхний предохранитель попытки — полный тайм-бюджет при мин. 8 КиБ/с.
///
/// `verify(strict)` — сверка «файл уже лежит целиком на диске?»:
///  • ДО заливки — verify(true), строгая (только по хэшу): seal_bytes
///    детерминирован, идентичный архив уже в облаке → пропуск заливки
///    целиком (0 байт трафика). Возврат Ok(false) = «не заливали».
///  • ПОСЛЕ неудачной попытки — verify(false), нестрогая: по истории диска
///    файл при «молчании сервера» УЖЕ лежит, потерян лишь ответ на PUT —
///    совпадение засчитывает попытку без ре-заливки 40 МБ.
/// Ошибка самой сверки → false → обычный ретрай.
async fn resilient_put<Req, Ver, VerFut>(
    make_req: Req,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
    verify: Ver,
) -> Result<bool, StorageError>
where
    Req: Fn() -> reqwest::RequestBuilder,
    Ver: Fn(bool) -> VerFut,
    VerFut: std::future::Future<Output = Result<bool, StorageError>>,
{
    if verify(true).await.unwrap_or(false) {
        return Ok(false);
    }
    let total = data.len() as u64;
    let buf = bytes::Bytes::from(data);
    let cb: Option<ProgressCb> = on_progress.map(std::sync::Arc::new);
    let attempt_cap = std::time::Duration::from_secs(60 + total / (8 * 1024));
    let mut last: Option<StorageError> = None;
    for attempt in 1..=3u32 {
        if attempt > 1 {
            tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
        }
        let req = make_req().timeout(attempt_cap);
        match put_with_stall_guard(req, buf.clone(), attempt, cb.clone()).await {
            Ok(r) if matches!(r.status().as_u16(), 200 | 201 | 204) => return Ok(true),
            Ok(r) => {
                let status = r.status().as_u16();
                let err = api_err(r).await;
                if is_permanent(status) {
                    return Err(err);
                }
                if verify(false).await.unwrap_or(false) {
                    return Ok(true);
                }
                last = Some(err);
            }
            Err(e) => {
                if verify(false).await.unwrap_or(false) {
                    return Ok(true);
                }
                last = Some(e);
            }
        }
    }
    Err(last.expect("минимум одна попытка PUT"))
}

/// 4xx-ошибки (кроме 408/429) — не сетевые: повтор бессмысленен.
fn is_permanent(status: u16) -> bool {
    (400..500).contains(&status) && status != 408 && status != 429
}

async fn api_err(resp: reqwest::Response) -> StorageError {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    StorageError::Api { status, body: body.chars().take(300).collect() }
}

/// Автор: создаёт /ntfryseed, заливает шифроблоки, публикует папку и
/// возвращает её public_key (это и есть folder_token для URI).
pub async fn yandex_upload_seed(
    token: &str,
    files: &[(String, Vec<u8>)],
) -> Result<String, StorageError> {
    let client = http_client();
    let auth = format!("OAuth {token}");

    // 1. MKCOL /ntfryseed (201 создано, 405 уже есть — оба успех)
    let mkcol = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL валиден");
    let resp = client
        .request(mkcol, format!("{YA_WEBDAV}/{SEED_DIR}"))
        .header("Authorization", &auth)
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !matches!(resp.status().as_u16(), 200 | 201 | 405) {
        return Err(api_err(resp).await);
    }

    // 1b. MKCOL /ntfryseed/dapps — структурированная подпапка архивов dApp
    let mkcol2 = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL валиден");
    let resp = client
        .request(mkcol2, format!("{YA_WEBDAV}/{SEED_DIR}/{SEED_DAPPS_DIR}"))
        .header("Authorization", &auth)
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !matches!(resp.status().as_u16(), 200 | 201 | 405) {
        return Err(api_err(resp).await);
    }

    // 2. PUT шифроблоков (WebDAV — одна транзакция на файл).
    // Мелкие файлы: жёсткий таймаут 60с + до 3 попыток — обрыв мобильной
    // связи не подвешивает публикацию навсегда.
    for (name, data) in files {
        let mut last: Option<StorageError> = None;
        let mut done = false;
        for attempt in 1..=3u32 {
            if attempt > 1 {
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
            }
            let resp = client
                .put(format!("{YA_WEBDAV}/{SEED_DIR}/{name}"))
                .header("Authorization", &auth)
                .timeout(std::time::Duration::from_secs(60))
                .body(data.clone())
                .send()
                .await;
            match resp {
                Ok(r) if matches!(r.status().as_u16(), 200 | 201 | 204) => {
                    done = true;
                    break;
                }
                Ok(r) => {
                    let status = r.status().as_u16();
                    let err = api_err(r).await;
                    if is_permanent(status) {
                        return Err(err);
                    }
                    last = Some(err);
                }
                Err(e) => last = Some(e.into()),
            }
        }
        if !done {
            return Err(last.expect("минимум одна попытка PUT"));
        }
    }

    // 3. Публикация папки (открытый доступ по ссылке). Повторная — no-op.
    let resp = client
        .put(format!("{YA_API}/publish"))
        .query(&[("path", format!("disk:/{SEED_DIR}"))])
        .header("Authorization", &auth)
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !matches!(resp.status().as_u16(), 200 | 201) {
        return Err(api_err(resp).await);
    }

    // 4. Читаем public_key опубликованной папки
    #[derive(Deserialize)]
    struct Meta {
        public_key: Option<String>,
    }
    let resp = client
        .get(YA_API)
        .query(&[("path", format!("disk:/{SEED_DIR}")), ("fields", "public_key".into())])
        .header("Authorization", &auth)
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let meta: Meta = resp.json().await?;
    meta.public_key
        .filter(|k| !k.is_empty())
        .ok_or_else(|| StorageError::NotFound("public_key папки ntfryseed не выдан Яндексом".into()))
}

/// Загрузка одного зашифрованного архива dApp в `/ntfryseed/dapps/<name>`.
///
/// ⚠️ Yandex Disk API **не поддерживает** chunked / resumable upload:
/// upload-URL ожидает ОДИН PUT со всем телом файла. `202 Accepted` в
/// официальной документации означает «файл принят, но ещё не перемещён на
/// Диск» (server-side async processing), а НЕ «частичный чанк принят».
/// `412 Precondition Failed` возвращается, если `Content-Range` не покрывает
/// весь файл. Т.е. любая попытка резать файл на несколько PUT'ов
/// заканчивается либо 412, либо перезаписью файла последним чанком.
///
/// Для устойчивости на мобильных сетях используем:
///  • WebDAV endpoint (`webdav.yandex.ru`) — без 30-мин TTL, авторизация
///    Bearer-токеном на каждый запрос;
///  • upload_client БЕЗ read_timeout + сторожок прогресса (см. resilient_put):
///    попытка живёт, пока байты уходят — 3G/EDGE без потолка длительности;
///  • явный `Content-Length` (WebDAV не принимает chunked без длины);
///  • до 3 попыток c бэкоффом 4/6с; прогресс новой попытки стартует с 0;
///  • `on_progress(sent, attempt)` — attempt (1..=3) для бейджа «попытка N/3».
///
/// Настоящий resume архитектурно возможен только через Yandex Object Storage
/// (S3-совместимый multipart upload), но это отдельный платный сервис.
/// Возврат: true — архив залит по сети; false — идентичный архив уже лежал
/// в облаке (sha256 совпал), заливка пропущена целиком.
/// Заливка одного медиа-файла каталога (logo/preview, ≤256 KB) в
/// `/ntfryseed/discovery_cache/<name>` — облачный fallback картинок.
pub async fn yandex_upload_media_file(
    token: &str,
    name: &str,
    data: Vec<u8>,
) -> Result<bool, StorageError> {
    let ctl = http_client();
    let auth = format!("OAuth {token}");
    for dir in [
        format!("{YA_WEBDAV}/{SEED_DIR}"),
        format!("{YA_WEBDAV}/{SEED_DIR}/{SEED_MEDIA_DIR}"),
    ] {
        let mkcol = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL валиден");
        let _ = ctl
            .request(mkcol, dir)
            .header("Authorization", &auth)
            .timeout(CTL_TIMEOUT)
            .send()
            .await;
    }
    let url = format!("{YA_WEBDAV}/{SEED_DIR}/{SEED_MEDIA_DIR}/{name}");
    let resp = upload_client()
        .put(&url)
        .header("Authorization", &auth)
        .body(data)
        .send()
        .await?;
    if resp.status().is_success() {
        Ok(true)
    } else {
        Err(api_err(resp).await)
    }
}

/// Аналог для Mail.ru (WebDAV, basic-auth `email:пароль_приложения`).
pub async fn mailru_upload_media_file(
    creds: &str,
    name: &str,
    data: Vec<u8>,
) -> Result<bool, StorageError> {
    let (login, password) = crate::storage::mailru::split_creds(creds)?;
    let (login, password) = (login.to_string(), password.to_string());
    let ctl = http_client();
    for dir in [
        format!("/{SEED_DIR}"),
        format!("/{SEED_DIR}/{SEED_MEDIA_DIR}"),
    ] {
        let mkcol = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL валиден");
        let _ = ctl
            .request(mkcol, format!("{MR_WEBDAV}{dir}"))
            .basic_auth(&login, Some(&password))
            .timeout(CTL_TIMEOUT)
            .send()
            .await;
    }
    let url = format!("{MR_WEBDAV}/{SEED_DIR}/{SEED_MEDIA_DIR}/{name}");
    let resp = upload_client()
        .put(&url)
        .basic_auth(&login, Some(&password))
        .body(data)
        .send()
        .await?;
    if resp.status().is_success() {
        Ok(true)
    } else {
        Err(api_err(resp).await)
    }
}

/// Аналог для Google Drive (папка /ntfryseed/discovery_cache по id).
pub async fn gdrive_upload_media_file(
    token: &str,
    name: &str,
    data: Vec<u8>,
) -> Result<bool, StorageError> {
    let client = http_client();
    let root = gdrive_ensure_folder(&client, token, SEED_DIR, "root").await?;
    let media = gdrive_ensure_folder(&client, token, SEED_MEDIA_DIR, &root).await?;
    gdrive_upsert_large(token, name, &media, data, None).await
}

pub async fn yandex_upload_seed_file(
    token: &str,
    name: &str,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
) -> Result<bool, StorageError> {
    let client = upload_client();
    let ctl = http_client();
    let url = format!("{YA_WEBDAV}/{SEED_DIR}/{SEED_DAPPS_DIR}/{name}");
    let auth = format!("OAuth {token}");
    let total = data.len() as u64;
    let sha_hex = hex::encode(Sha256::digest(&data));
    resilient_put(
        || client.put(&url).header("Authorization", &auth),
        data,
        on_progress,
        |strict| yandex_seed_file_matches(&ctl, token, name, total, &sha_hex, strict),
    )
    .await
}

/// «Файл уже лёг целиком?» — дешёвый GET метаданных (size + sha256).
/// strict=true (пре-чек до заливки): засчитываем ТОЛЬКО точное совпадение
/// размера и sha256 — seal_bytes детерминирован, так что неизменившийся
/// архив узнаётся байт-в-байт. strict=false (после молчания сервера):
/// sha256 свежего файла может быть ещё не посчитан Яндексом — тогда
/// достаточно размера (старый файл того же имени имел бы готовый хэш).
/// Любая ошибка запроса → false (обычный ретрай).
async fn yandex_seed_file_matches(
    client: &reqwest::Client,
    token: &str,
    name: &str,
    size: u64,
    sha256_hex: &str,
    strict: bool,
) -> Result<bool, StorageError> {
    #[derive(Deserialize)]
    struct Meta {
        size: Option<u64>,
        sha256: Option<String>,
    }
    let resp = client
        .get(YA_API)
        .query(&[
            ("path", format!("disk:/{SEED_DIR}/{SEED_DAPPS_DIR}/{name}")),
            ("fields", "size,sha256".into()),
        ])
        .header("Authorization", format!("OAuth {token}"))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Ok(false);
    }
    let m: Meta = resp.json().await?;
    if m.size != Some(size) {
        return Ok(false);
    }
    Ok(match m.sha256.as_deref() {
        Some(h) => h.eq_ignore_ascii_case(sha256_hex),
        None => !strict,
    })
}

/// Автор: удаление legacy-архива dApp из корня /ntfryseed (миграция с плоской
/// раскладки на /ntfryseed/dapps/<id>.enc). 404 → Ok(()); остальные ошибки —
/// логируются вызывающей стороной, но не валят публикацию.
/// Автор: удаление архива dApp из /ntfryseed/dapps на Яндекс Диске
/// (после локального удаления приложения пользователем). 404 → NotFound.
pub async fn yandex_delete_seed_dapp_file(
    token: &str,
    name: &str,
) -> Result<(), StorageError> {
    let client = http_client();
    let resp = client
        .delete(YA_API)
        .query(&[
            ("path", format!("disk:/{SEED_DIR}/{SEED_DAPPS_DIR}/{name}")),
            ("permanently", "true".into()),
        ])
        .header("Authorization", format!("OAuth {token}"))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    match resp.status().as_u16() {
        200 | 202 | 204 => Ok(()),
        404 => Err(StorageError::NotFound(name.to_string())),
        _ => Err(api_err(resp).await),
    }
}

pub async fn yandex_delete_seed_root_file(
    token: &str,
    name: &str,
) -> Result<(), StorageError> {
    let client = http_client();
    let resp = client
        .delete(YA_API)
        .query(&[("path", format!("disk:/{SEED_DIR}/{name}")), ("permanently", "true".into())])
        .header("Authorization", format!("OAuth {token}"))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    match resp.status().as_u16() {
        200 | 202 | 204 => Ok(()),
        404 => Err(StorageError::NotFound(name.to_string())),
        _ => Err(api_err(resp).await),
    }
}

/// Пир: анонимно скачивает файл из чужой опубликованной папки —
/// БЕЗ OAuth, только по public_key папки (двухфазно: href → байты).
pub async fn yandex_fetch_public(
    folder_public_key: &str,
    file: &str,
) -> Result<Vec<u8>, StorageError> {
    let client = http_client();
    #[derive(Deserialize)]
    struct Href {
        href: String,
    }
    let resp = client
        .get(format!("{YA_PUBLIC}/download"))
        .query(&[("public_key", folder_public_key), ("path", &format!("/{file}"))])
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if resp.status().as_u16() == 404 {
        return Err(StorageError::NotFound(file.to_string()));
    }
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let href: Href = resp.json().await?;
    let resp = client.get(&href.href).send().await?;
    if resp.status().as_u16() == 404 {
        return Err(StorageError::NotFound(file.to_string()));
    }
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    Ok(resp.bytes().await?.to_vec())
}

// ── Облако Mail.ru: выгрузка автором (WebDAV) + анонимное чтение пирами ────
//    У Mail.ru нет официального публичного REST API: публикация папки и
//    анонимное скачивание идут через неофициальные endpoint'ы (o2.mail.ru
//    password-grant + api/m1 + страница /public/<weblink>), как у сторонних
//    клиентов (WebDavMailRuCloud и др.). WebDAV — только на платных тарифах.

const MR_WEBDAV: &str = "https://webdav.cloud.mail.ru";
const MR_M1: &str = "https://cloud.mail.ru/api/m1";
const MR_PUBLIC: &str = "https://cloud.mail.ru/public";

/// Обрезка тела ответа для диагностики в ошибках.
fn snip(s: &str) -> String {
    s.chars().take(220).collect()
}

/// access_token неофициального API по email+пароль_приложения
/// (тот же password-grant, что у квоты в storage/mailru.rs).
/// Ошибки НЕ глотаются: тело ответа o2 попадает в текст ошибки —
/// у VK ID-логинов (@vk.com) password-grant может быть недоступен.
async fn mailru_access_token(
    client: &reqwest::Client,
    login: &str,
    password: &str,
) -> Result<String, StorageError> {
    let resp = client
        .post("https://o2.mail.ru/token")
        .form(&[
            ("client_id", "cloud-win"),
            ("grant_type", "password"),
            ("username", login),
            ("password", password),
        ])
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|_| StorageError::Api {
        status,
        body: format!("o2.mail.ru/token: не-JSON ответ: {}", snip(&text)),
    })?;
    if let Some(tok) = v.get("access_token").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) {
        return Ok(tok.to_string());
    }
    Err(StorageError::Api {
        status,
        body: format!(
            "o2.mail.ru/token: нет access_token (логины VK ID могут не поддерживаться): {}",
            snip(&text)
        ),
    })
}

/// Автор: создаёт /ntfryseed на Облаке Mail.ru (WebDAV, Basic-auth),
/// заливает шифроблоки, публикует папку через api/m1 и возвращает её
/// weblink (это и есть folder_token для URI sn://seed/m@…).
/// `known_weblink`: weblink уже опубликованной папки (ручная публикация или
/// прошлый publish) — тогда шаг o2/api-m1 пропускается ПОЛНОСТЬЮ (важно для
/// VK ID-логинов, где password-grant может быть недоступен).
pub async fn mailru_upload_seed(
    creds: &str,
    files: &[(String, Vec<u8>)],
    known_weblink: Option<&str>,
) -> Result<String, StorageError> {
    let (login, password) = crate::storage::mailru::split_creds(creds)?;
    let client = http_client();

    // 1. MKCOL /ntfryseed и /ntfryseed/dapps (201 создано, 405 уже есть)
    for dir in [format!("/{SEED_DIR}"), format!("/{SEED_DIR}/{SEED_DAPPS_DIR}")] {
        let mkcol = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL валиден");
        let resp = client
            .request(mkcol, format!("{MR_WEBDAV}{dir}"))
            .basic_auth(login, Some(password))
            .timeout(CTL_TIMEOUT)
            .send()
            .await?;
        if !matches!(resp.status().as_u16(), 200 | 201 | 405) {
            return Err(api_err(resp).await);
        }
    }

    // 2. PUT шифроблоков (мелкие) — до 3 попыток на файл
    for (name, data) in files {
        let mut last: Option<StorageError> = None;
        let mut done = false;
        for attempt in 1..=3u32 {
            if attempt > 1 {
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
            }
            let resp = client
                .put(format!("{MR_WEBDAV}/{SEED_DIR}/{name}"))
                .basic_auth(login, Some(password))
                .timeout(std::time::Duration::from_secs(60))
                .body(data.clone())
                .send()
                .await;
            match resp {
                Ok(r) if matches!(r.status().as_u16(), 200 | 201 | 204) => {
                    done = true;
                    break;
                }
                Ok(r) => {
                    let status = r.status().as_u16();
                    let err = api_err(r).await;
                    if is_permanent(status) {
                        return Err(err);
                    }
                    last = Some(err);
                }
                Err(e) => last = Some(e.into()),
            }
        }
        if !done {
            return Err(last.expect("минимум одна попытка PUT"));
        }
    }

    // 3. Weblink уже известен (ручная публикация / прошлый publish) —
    // o2-токен и api/m1 не нужны: обновлённые файлы уже лежат в папке.
    if let Some(w) = known_weblink.map(str::trim).filter(|w| !w.is_empty()) {
        return Ok(w.to_string());
    }

    // 3b. Публикация папки → weblink. Повторный publish у Mail.ru возвращает
    // существующий weblink либо ошибку — тогда читаем weblink из меты файла.
    let token = mailru_access_token(&client, login, password).await?;
    let resp = client
        .post(format!("{MR_M1}/file/publish"))
        .query(&[("access_token", token.as_str())])
        .form(&[("home", format!("/{SEED_DIR}")), ("email", login.to_string()), ("api", "2".into())])
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    let pub_status = resp.status().as_u16();
    let pub_body = resp.text().await.unwrap_or_default();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&pub_body) {
        if let Some(weblink) = v.get("body").and_then(|b| b.as_str()).filter(|s| !s.is_empty()) {
            return Ok(weblink.to_string());
        }
    }
    // Фолбэк: GET /api/m1/file — weblink уже опубликованной папки
    let resp = client
        .get(format!("{MR_M1}/file"))
        .query(&[("access_token", token.as_str()), ("home", &format!("/{SEED_DIR}"))])
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    let meta_status = resp.status().as_u16();
    let meta_body = resp.text().await.unwrap_or_default();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&meta_body) {
        if let Some(weblink) = v
            .get("body")
            .and_then(|b| b.get("weblink"))
            .and_then(|w| w.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(weblink.to_string());
        }
    }
    // Оба пути без weblink — отдаём полную диагностику обоих ответов
    Err(StorageError::Api {
        status: pub_status,
        body: format!(
            "Mail.ru не выдал weblink папки /ntfryseed. publish({pub_status}): {} | meta({meta_status}): {} — опубликуйте папку вручную в веб-интерфейсе Облака",
            snip(&pub_body),
            snip(&meta_body)
        ),
    })
}

/// Загрузка одного (крупного) архива dApp в /ntfryseed/dapps на Mail.ru
/// (WebDAV). Та же устойчивость, что у Яндекса: upload_client без
/// read_timeout, сторожок прогресса, до 3 попыток, `on_progress(sent, attempt)`.
/// Возврат — как у yandex_upload_seed_file (false = заливка пропущена).
pub async fn mailru_upload_seed_file(
    creds: &str,
    name: &str,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
) -> Result<bool, StorageError> {
    let (login, password) = crate::storage::mailru::split_creds(creds)?;
    let (login, password) = (login.to_string(), password.to_string());
    let client = upload_client();
    let ctl = http_client();
    let url = format!("{MR_WEBDAV}/{SEED_DIR}/{SEED_DAPPS_DIR}/{name}");
    let total = data.len() as u64;
    resilient_put(
        || client.put(&url).basic_auth(&login, Some(&password)),
        data,
        on_progress,
        |strict| mailru_seed_file_matches(&ctl, &login, &password, name, total, strict),
    )
    .await
}

/// Как yandex_seed_file_matches, но Mail.ru WebDAV хэш не отдаёт — сверяем
/// только размер (PROPFIND Depth:0, getcontentlength). strict (пре-чек до
/// заливки) всегда false: пропуск заливки лишь по размеру небезопасен —
/// мог бы молча оставить в облаке старую версию архива. После неудачной
/// попытки контекст другой: файл точного размера писали только что мы.
async fn mailru_seed_file_matches(
    client: &reqwest::Client,
    login: &str,
    password: &str,
    name: &str,
    size: u64,
    strict: bool,
) -> Result<bool, StorageError> {
    if strict {
        return Ok(false);
    }
    let propfind = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND валиден");
    let resp = client
        .request(propfind, format!("{MR_WEBDAV}/{SEED_DIR}/{SEED_DAPPS_DIR}/{name}"))
        .basic_auth(login, Some(password))
        .header("Depth", "0")
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !matches!(resp.status().as_u16(), 200 | 207) {
        return Ok(false);
    }
    let xml = resp.text().await?;
    Ok(crate::storage::mailru::xml_num(&xml, "getcontentlength") == Some(size))
}

/// Автор: удаление архива dApp из /ntfryseed/dapps на Mail.ru (WebDAV DELETE).
pub async fn mailru_delete_seed_dapp_file(creds: &str, name: &str) -> Result<(), StorageError> {
    let (login, password) = crate::storage::mailru::split_creds(creds)?;
    let client = http_client();
    let resp = client
        .delete(format!("{MR_WEBDAV}/{SEED_DIR}/{SEED_DAPPS_DIR}/{name}"))
        .basic_auth(login, Some(password))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    match resp.status().as_u16() {
        200 | 202 | 204 => Ok(()),
        404 => Err(StorageError::NotFound(name.to_string())),
        _ => Err(api_err(resp).await),
    }
}

/// Автор: удаление legacy-архива из корня /ntfryseed на Mail.ru (WebDAV DELETE).
pub async fn mailru_delete_seed_root_file(creds: &str, name: &str) -> Result<(), StorageError> {
    let (login, password) = crate::storage::mailru::split_creds(creds)?;
    let client = http_client();
    let resp = client
        .delete(format!("{MR_WEBDAV}/{SEED_DIR}/{name}"))
        .basic_auth(login, Some(password))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    match resp.status().as_u16() {
        200 | 202 | 204 => Ok(()),
        404 => Err(StorageError::NotFound(name.to_string())),
        _ => Err(api_err(resp).await),
    }
}

// ── Generic subpath API: произвольные вложения внутри /ntfryseed ────────────
//
// Используется профильным сидером (users/<id>.enc, users/<id>/files/<cid>.enc),
// но написано максимально нейтрально — годится под любой будущий подслой.
// Всегда создаёт недостающие родительские папки (MKCOL идемпотентен).

/// Нормализация subpath: убирает ведущие `/`, режет `..`, отбрасывает пустые
/// сегменты. Возвращает Ok(vec[сегменты]) или ошибку, если путь пуст.
fn split_seed_subpath(subpath: &str) -> Result<Vec<String>, StorageError> {
    let clean: Vec<String> = subpath
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_string)
        .collect();
    if clean.is_empty() {
        return Err(StorageError::Api {
            status: 400,
            body: "пустой subpath".into(),
        });
    }
    if clean.iter().any(|s| s == "..") {
        return Err(StorageError::Api {
            status: 400,
            body: "'..' в subpath запрещён (обход /ntfryseed)".into(),
        });
    }
    Ok(clean)
}

/// Создать всю цепочку папок для будущего файла subpath на Яндексе (WebDAV MKCOL).
/// 201/405 — успех; всё остальное — ошибка. Родитель `SEED_DIR` предполагается
/// уже созданным вызывающей стороной или прошлыми публикациями.
async fn yandex_ensure_dirs(
    client: &reqwest::Client,
    token: &str,
    parts: &[String],
) -> Result<(), StorageError> {
    if parts.len() <= 1 {
        return Ok(()); // файл лежит прямо в /ntfryseed
    }
    let auth = format!("OAuth {token}");
    let mut acc = String::new();
    // parts[..parts.len()-1] — только каталоги
    for p in &parts[..parts.len() - 1] {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(p);
        let mkcol = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL валиден");
        let resp = client
            .request(mkcol, format!("{YA_WEBDAV}/{SEED_DIR}/{acc}"))
            .header("Authorization", &auth)
            .timeout(CTL_TIMEOUT)
            .send()
            .await?;
        if !matches!(resp.status().as_u16(), 200 | 201 | 405) {
            return Err(api_err(resp).await);
        }
    }
    Ok(())
}

async fn mailru_ensure_dirs(
    client: &reqwest::Client,
    login: &str,
    password: &str,
    parts: &[String],
) -> Result<(), StorageError> {
    if parts.len() <= 1 {
        return Ok(());
    }
    let mut acc = String::new();
    for p in &parts[..parts.len() - 1] {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(p);
        let mkcol = reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL валиден");
        let resp = client
            .request(mkcol, format!("{MR_WEBDAV}/{SEED_DIR}/{acc}"))
            .basic_auth(login, Some(password))
            .timeout(CTL_TIMEOUT)
            .send()
            .await?;
        if !matches!(resp.status().as_u16(), 200 | 201 | 405) {
            return Err(api_err(resp).await);
        }
    }
    Ok(())
}

/// Автор (Яндекс): заливка данных в /ntfryseed/<subpath> с той же устойчивостью,
/// что у dApp-архивов (upload_client, сторожок прогресса, до 3 попыток,
/// sha256 pre-check, size fallback). Отсутствующие подпапки создаются.
/// Возврат: true — залито по сети; false — идентичный файл уже был на диске.
pub async fn yandex_upload_subpath(
    token: &str,
    subpath: &str,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
) -> Result<bool, StorageError> {
    let parts = split_seed_subpath(subpath)?;
    let joined = parts.join("/");
    let client = upload_client();
    let ctl = http_client();
    yandex_ensure_dirs(&ctl, token, &parts).await?;
    let url = format!("{YA_WEBDAV}/{SEED_DIR}/{joined}");
    let auth = format!("OAuth {token}");
    let total = data.len() as u64;
    let sha_hex = hex::encode(Sha256::digest(&data));
    resilient_put(
        || client.put(&url).header("Authorization", &auth),
        data,
        on_progress,
        |strict| yandex_subpath_matches(&ctl, token, &joined, total, &sha_hex, strict),
    )
    .await
}

async fn yandex_subpath_matches(
    client: &reqwest::Client,
    token: &str,
    subpath: &str,
    size: u64,
    sha256_hex: &str,
    strict: bool,
) -> Result<bool, StorageError> {
    #[derive(Deserialize)]
    struct Meta {
        size: Option<u64>,
        sha256: Option<String>,
    }
    let resp = client
        .get(YA_API)
        .query(&[
            ("path", format!("disk:/{SEED_DIR}/{subpath}")),
            ("fields", "size,sha256".into()),
        ])
        .header("Authorization", format!("OAuth {token}"))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Ok(false);
    }
    let m: Meta = resp.json().await?;
    if m.size != Some(size) {
        return Ok(false);
    }
    Ok(match m.sha256.as_deref() {
        Some(h) => h.eq_ignore_ascii_case(sha256_hex),
        None => !strict,
    })
}

/// Автор (Яндекс): удаление произвольного subpath из /ntfryseed. 404 → NotFound.
pub async fn yandex_delete_subpath(token: &str, subpath: &str) -> Result<(), StorageError> {
    let parts = split_seed_subpath(subpath)?;
    let joined = parts.join("/");
    let client = http_client();
    let resp = client
        .delete(YA_API)
        .query(&[
            ("path", format!("disk:/{SEED_DIR}/{joined}")),
            ("permanently", "true".into()),
        ])
        .header("Authorization", format!("OAuth {token}"))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    match resp.status().as_u16() {
        200 | 202 | 204 => Ok(()),
        404 => Err(StorageError::NotFound(joined)),
        _ => Err(api_err(resp).await),
    }
}

/// Автор (Mail.ru): заливка данных в /ntfryseed/<subpath> через WebDAV.
/// pre-check по sha256 у Mail.ru невозможен (WebDAV не отдаёт хэш) — стратегия
/// та же, что у mailru_upload_seed_file: strict=false после неудачной попытки.
pub async fn mailru_upload_subpath(
    creds: &str,
    subpath: &str,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
) -> Result<bool, StorageError> {
    let (login, password) = crate::storage::mailru::split_creds(creds)?;
    let (login, password) = (login.to_string(), password.to_string());
    let parts = split_seed_subpath(subpath)?;
    let joined = parts.join("/");
    let client = upload_client();
    let ctl = http_client();
    mailru_ensure_dirs(&ctl, &login, &password, &parts).await?;
    let url = format!("{MR_WEBDAV}/{SEED_DIR}/{joined}");
    let total = data.len() as u64;
    resilient_put(
        || client.put(&url).basic_auth(&login, Some(&password)),
        data,
        on_progress,
        |strict| mailru_subpath_matches(&ctl, &login, &password, &joined, total, strict),
    )
    .await
}

async fn mailru_subpath_matches(
    client: &reqwest::Client,
    login: &str,
    password: &str,
    subpath: &str,
    size: u64,
    strict: bool,
) -> Result<bool, StorageError> {
    if strict {
        return Ok(false);
    }
    let propfind = reqwest::Method::from_bytes(b"PROPFIND").expect("PROPFIND валиден");
    let resp = client
        .request(propfind, format!("{MR_WEBDAV}/{SEED_DIR}/{subpath}"))
        .basic_auth(login, Some(password))
        .header("Depth", "0")
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !matches!(resp.status().as_u16(), 200 | 207) {
        return Ok(false);
    }
    let xml = resp.text().await?;
    Ok(crate::storage::mailru::xml_num(&xml, "getcontentlength") == Some(size))
}

/// Автор (Mail.ru): удаление произвольного subpath из /ntfryseed (WebDAV DELETE).
pub async fn mailru_delete_subpath(creds: &str, subpath: &str) -> Result<(), StorageError> {
    let (login, password) = crate::storage::mailru::split_creds(creds)?;
    let parts = split_seed_subpath(subpath)?;
    let joined = parts.join("/");
    let client = http_client();
    let resp = client
        .delete(format!("{MR_WEBDAV}/{SEED_DIR}/{joined}"))
        .basic_auth(login, Some(password))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    match resp.status().as_u16() {
        200 | 202 | 204 => Ok(()),
        404 => Err(StorageError::NotFound(joined)),
        _ => Err(api_err(resp).await),
    }
}

/// Достаёт `"url":"…"` после маркера (парсинг встраиваемого JSON без serde —
/// страница /public/<weblink> отдаёт огромный HTML со state-объектом внутри).
fn extract_url_after(text: &str, marker: &str) -> Option<String> {
    let pos = text.find(marker)?;
    let rest = &text[pos..];
    let upos = rest.find("\"url\":\"")?;
    let rest = &rest[upos + 7..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// База скачивания публичных weblink'ов: анонимный /api/v2/dispatcher,
/// фолбэк — парсинг HTML публичной страницы (там же лежит weblink_get).
async fn mailru_weblink_get_base(
    client: &reqwest::Client,
    weblink: &str,
) -> Result<String, StorageError> {
    if let Ok(resp) = client
        .get("https://cloud.mail.ru/api/v2/dispatcher")
        .query(&[("api", "2")])
        .timeout(CTL_TIMEOUT)
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                if let Some(url) = v
                    .get("body")
                    .and_then(|b| b.get("weblink_get"))
                    .and_then(|w| w.get(0))
                    .and_then(|e| e.get("url"))
                    .and_then(|u| u.as_str())
                {
                    return Ok(url.trim_end_matches('/').to_string());
                }
            }
        }
    }
    // Фолбэк: HTML публичной страницы содержит "weblink_get":[{"count":…,"url":"…"}]
    let resp = client
        .get(format!("{MR_PUBLIC}/{weblink}"))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if resp.status().as_u16() == 404 {
        return Err(StorageError::NotFound(format!("публичная папка {weblink}")));
    }
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let html = resp.text().await?;
    extract_url_after(&html, "\"weblink_get\"")
        .map(|u| u.trim_end_matches('/').to_string())
        .ok_or_else(|| {
            StorageError::NotFound("weblink_get не найден на публичной странице Mail.ru".into())
        })
}

/// Анонимный download-токен (?key=…). Отсутствие токена — не фатально:
/// часть зеркал отдаёт файлы и без key.
async fn mailru_download_key(client: &reqwest::Client) -> Option<String> {
    let v: serde_json::Value = client
        .get("https://cloud.mail.ru/api/v2/tokens/download")
        .timeout(CTL_TIMEOUT)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("body")
        .and_then(|b| b.get("token").or_else(|| b.as_str().map(|_| b)))
        .and_then(|t| t.as_str())
        .map(str::to_string)
}

/// Пир: анонимно скачивает файл из чужой опубликованной папки Mail.ru —
/// БЕЗ логина, только по weblink папки: {weblink_get}/{weblink}/{file}?key=….
pub async fn mailru_fetch_public(weblink: &str, file: &str) -> Result<Vec<u8>, StorageError> {
    let client = http_client();
    let base = mailru_weblink_get_base(&client, weblink).await?;
    let key = mailru_download_key(&client).await;
    let mut req = client.get(format!("{base}/{weblink}/{file}"));
    if let Some(k) = &key {
        req = req.query(&[("key", k.as_str())]);
    }
    let resp = req.send().await?;
    if matches!(resp.status().as_u16(), 404 | 400) {
        return Err(StorageError::NotFound(file.to_string()));
    }
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let bytes = resp.bytes().await?.to_vec();
    // Mail.ru на несуществующий путь может отдать HTML-страницу вместо 404
    if bytes.starts_with(b"<!DOCTYPE") || bytes.starts_with(b"<html") {
        return Err(StorageError::NotFound(file.to_string()));
    }
    Ok(bytes)
}

/// Публичная ССЫЛКА Mail.ru на ОДИН файл → прямой download-URL (анонимно, без
/// логина). Вход: `https://cloud.mail.ru/public/<weblink>`. На выходе —
/// `{weblink_get}/{weblink}?key=…`, который можно стримить любым HTTP-клиентом
/// (используется как «cloud mode» fallback для больших файлов, напр. GGUF-модели).
pub async fn mailru_public_direct_url(public_url: &str) -> Result<String, StorageError> {
    let s = public_url.trim();
    let weblink = s
        .split("/public/")
        .nth(1)
        .unwrap_or("")
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches('/');
    if weblink.is_empty() {
        return Err(StorageError::NotFound(format!(
            "не разобрал weblink из публичной ссылки Mail.ru: {s}"
        )));
    }
    let client = http_client();
    let base = mailru_weblink_get_base(&client, weblink).await?;
    let mut url = format!("{base}/{weblink}");
    if let Some(k) = mailru_download_key(&client).await {
        url.push_str("?key=");
        url.push_str(&k);
    }
    Ok(url)
}

// ── Google Drive: выгрузка автором (Drive API v3) + анонимное чтение пирами ─
//    Автор: OAuth-токен (scope drive.file) — папка ntfryseed создаётся в корне
//    Диска и открывается «всем, у кого есть ссылка» (reader). folder_token в
//    URI sn://seed/g@… = ID этой папки.
//    Пиры: официального анонимного API у Google нет — листинг публичной папки
//    идёт через HTML embeddedfolderview, скачивание файла по id — через
//    drive.usercontent.google.com/download (та же модель, что weblink у Mail.ru).

const GD_API: &str = "https://www.googleapis.com/drive/v3/files";
const GD_UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const GD_FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const GD_VIEW: &str = "https://drive.google.com/embeddedfolderview";
const GD_DOWNLOAD: &str = "https://drive.usercontent.google.com/download";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GdMeta {
    id: String,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    sha256_checksum: Option<String>,
}

#[derive(Deserialize)]
struct GdList {
    #[serde(default)]
    files: Vec<GdMeta>,
}

/// Экранирование имени для q-запроса Drive API (одинарные кавычки/бэкслэш).
fn gd_q_escape(name: &str) -> String {
    name.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Поиск файла/папки по имени внутри родителя (trashed=false).
async fn gdrive_find(
    client: &reqwest::Client,
    token: &str,
    name: &str,
    parent: &str,
    fields: &str,
) -> Result<Option<GdMeta>, StorageError> {
    let q = format!("name='{}' and '{parent}' in parents and trashed=false", gd_q_escape(name));
    let resp = client
        .get(GD_API)
        .bearer_auth(token)
        .query(&[
            ("q", q.as_str()),
            ("fields", &format!("files({fields})") as &str),
            ("pageSize", "1"),
        ])
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let list: GdList = resp.json().await?;
    Ok(list.files.into_iter().next())
}

async fn gdrive_create_folder(
    client: &reqwest::Client,
    token: &str,
    name: &str,
    parent: &str,
) -> Result<String, StorageError> {
    let resp = client
        .post(GD_API)
        .bearer_auth(token)
        .query(&[("fields", "id")])
        .json(&serde_json::json!({ "name": name, "mimeType": GD_FOLDER_MIME, "parents": [parent] }))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let meta: GdMeta = resp.json().await?;
    Ok(meta.id)
}

/// Гарантирует папку `name` внутри parent (создаёт при отсутствии) → ID.
async fn gdrive_ensure_folder(
    client: &reqwest::Client,
    token: &str,
    name: &str,
    parent: &str,
) -> Result<String, StorageError> {
    match gdrive_find(client, token, name, parent, "id").await? {
        Some(m) => Ok(m.id),
        None => gdrive_create_folder(client, token, name, parent).await,
    }
}

/// multipart/related тело для создания файла (метаданные JSON + бинарь).
fn gd_multipart_body(name: &str, parent: &str, data: &[u8]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "ntfryseed_multipart_boundary";
    let meta = serde_json::json!({ "name": name, "parents": [parent] }).to_string();
    let mut body: Vec<u8> = Vec::with_capacity(data.len() + meta.len() + 256);
    body.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--").as_bytes());
    (format!("multipart/related; boundary={BOUNDARY}"), body)
}

/// Создать/перезаписать МЕЛКИЙ файл по имени (до 3 попыток, таймаут 60с).
/// Поиск существующего файла повторяется на каждой попытке — защита от
/// дублей, когда сервер создал файл, а ответ на POST потерялся.
async fn gdrive_upsert_small(
    client: &reqwest::Client,
    token: &str,
    name: &str,
    parent: &str,
    data: &[u8],
) -> Result<(), StorageError> {
    let mut last: Option<StorageError> = None;
    for attempt in 1..=3u32 {
        if attempt > 1 {
            tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
        }
        let existing = gdrive_find(client, token, name, parent, "id").await?;
        let resp = match &existing {
            Some(m) => {
                client
                    .patch(format!("{GD_UPLOAD}/{}", m.id))
                    .query(&[("uploadType", "media")])
                    .bearer_auth(token)
                    .header("Content-Type", "application/octet-stream")
                    .timeout(std::time::Duration::from_secs(60))
                    .body(data.to_vec())
                    .send()
                    .await
            }
            None => {
                let (ct, body) = gd_multipart_body(name, parent, data);
                client
                    .post(GD_UPLOAD)
                    .query(&[("uploadType", "multipart"), ("fields", "id")])
                    .bearer_auth(token)
                    .header("Content-Type", ct)
                    .timeout(std::time::Duration::from_secs(60))
                    .body(body)
                    .send()
                    .await
            }
        };
        match resp {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => {
                let status = r.status().as_u16();
                let err = api_err(r).await;
                if is_permanent(status) {
                    return Err(err);
                }
                last = Some(err);
            }
            Err(e) => last = Some(e.into()),
        }
    }
    Err(last.expect("минимум одна попытка"))
}

/// «Файл уже лёг целиком?» — метаданные size + sha256Checksum через Drive API.
/// Семантика strict — как у yandex_seed_file_matches: строгий пре-чек требует
/// совпадения хэша, после «молчания сервера» достаточно размера, если хэш
/// облаком ещё не посчитан.
async fn gdrive_file_matches(
    client: &reqwest::Client,
    token: &str,
    name: &str,
    parent: &str,
    size: u64,
    sha256_hex: &str,
    strict: bool,
) -> Result<bool, StorageError> {
    let Ok(Some(m)) = gdrive_find(client, token, name, parent, "id,size,sha256Checksum").await
    else {
        return Ok(false);
    };
    if m.size.as_deref().and_then(|s| s.parse::<u64>().ok()) != Some(size) {
        return Ok(false);
    }
    Ok(match m.sha256_checksum.as_deref() {
        Some(h) => h.eq_ignore_ascii_case(sha256_hex),
        None => !strict,
    })
}

/// Заливка КРУПНОГО файла (архива) с той же устойчивостью, что у Яндекса:
/// upload_client без read_timeout, сторожок прогресса, до 3 попыток,
/// sha256 pre-check (идентичное содержимое пропускается без байтов трафика).
async fn gdrive_upsert_large(
    token: &str,
    name: &str,
    parent: &str,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
) -> Result<bool, StorageError> {
    let up = upload_client();
    let ctl = http_client();
    let total = data.len() as u64;
    let sha_hex = hex::encode(Sha256::digest(&data));
    match gdrive_find(&ctl, token, name, parent, "id").await? {
        Some(m) => {
            let url = format!("{GD_UPLOAD}/{}?uploadType=media", m.id);
            resilient_put(
                || {
                    up.patch(&url)
                        .bearer_auth(token)
                        .header("Content-Type", "application/octet-stream")
                },
                data,
                on_progress,
                |strict| gdrive_file_matches(&ctl, token, name, parent, total, &sha_hex, strict),
            )
            .await
        }
        None => {
            let (ct, body) = gd_multipart_body(name, parent, &data);
            resilient_put(
                || {
                    up.post(GD_UPLOAD)
                        .query(&[("uploadType", "multipart"), ("fields", "id")])
                        .bearer_auth(token)
                        .header("Content-Type", ct.clone())
                },
                body,
                on_progress,
                |strict| gdrive_file_matches(&ctl, token, name, parent, total, &sha_hex, strict),
            )
            .await
        }
    }
}

/// Доступ «всем, у кого есть ссылка» (reader) на папку. Повторный вызов, если
/// permission anyone уже есть, засчитывается успехом.
async fn gdrive_publish_folder(
    client: &reqwest::Client,
    token: &str,
    folder_id: &str,
) -> Result<(), StorageError> {
    let resp = client
        .post(format!("{GD_API}/{folder_id}/permissions"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "role": "reader", "type": "anyone" }))
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if resp.status().is_success() {
        return Ok(());
    }
    let err = api_err(resp).await;
    // возможно, доступ уже открыт — сверяем список permissions
    let check = client
        .get(format!("{GD_API}/{folder_id}"))
        .bearer_auth(token)
        .query(&[("fields", "permissions(type,role)")])
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if check.status().is_success() {
        if let Ok(v) = check.json::<serde_json::Value>().await {
            let has_anyone = v
                .get("permissions")
                .and_then(|p| p.as_array())
                .map(|a| a.iter().any(|p| p.get("type").and_then(|t| t.as_str()) == Some("anyone")))
                .unwrap_or(false);
            if has_anyone {
                return Ok(());
            }
        }
    }
    Err(err)
}

/// Автор: создаёт /ntfryseed (+dapps) на Google Drive, заливает шифроблоки,
/// открывает доступ «всем, у кого есть ссылка» и возвращает ID папки
/// (это и есть folder_token для URI sn://seed/g@…).
pub async fn gdrive_upload_seed(
    token: &str,
    files: &[(String, Vec<u8>)],
) -> Result<String, StorageError> {
    let client = http_client();
    let root = gdrive_ensure_folder(&client, token, SEED_DIR, "root").await?;
    gdrive_ensure_folder(&client, token, SEED_DAPPS_DIR, &root).await?;
    for (name, data) in files {
        gdrive_upsert_small(&client, token, name, &root, data).await?;
    }
    gdrive_publish_folder(&client, token, &root).await?;
    Ok(root)
}

/// Загрузка одного (крупного) архива dApp в /ntfryseed/dapps на Google Drive.
/// Возврат — как у yandex_upload_seed_file (false = заливка пропущена по sha256).
pub async fn gdrive_upload_seed_file(
    token: &str,
    name: &str,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
) -> Result<bool, StorageError> {
    let client = http_client();
    let root = gdrive_ensure_folder(&client, token, SEED_DIR, "root").await?;
    let dapps = gdrive_ensure_folder(&client, token, SEED_DAPPS_DIR, &root).await?;
    gdrive_upsert_large(token, name, &dapps, data, on_progress).await
}

/// Удаление файла по имени внутри родителя. Нет файла → NotFound.
async fn gdrive_delete_in(
    client: &reqwest::Client,
    token: &str,
    name: &str,
    parent: &str,
) -> Result<(), StorageError> {
    let Some(m) = gdrive_find(client, token, name, parent, "id").await? else {
        return Err(StorageError::NotFound(name.to_string()));
    };
    let resp = client
        .delete(format!("{GD_API}/{}", m.id))
        .bearer_auth(token)
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    match resp.status().as_u16() {
        200 | 202 | 204 | 404 => Ok(()),
        _ => Err(api_err(resp).await),
    }
}

/// ID папки /ntfryseed (без создания) — для delete-операций.
async fn gdrive_seed_root(
    client: &reqwest::Client,
    token: &str,
) -> Result<String, StorageError> {
    gdrive_find(client, token, SEED_DIR, "root", "id")
        .await?
        .map(|m| m.id)
        .ok_or_else(|| StorageError::NotFound(SEED_DIR.to_string()))
}

/// Автор: удаление архива dApp из /ntfryseed/dapps на Google Drive.
pub async fn gdrive_delete_seed_dapp_file(token: &str, name: &str) -> Result<(), StorageError> {
    let client = http_client();
    let root = gdrive_seed_root(&client, token).await?;
    let dapps = gdrive_find(&client, token, SEED_DAPPS_DIR, &root, "id")
        .await?
        .map(|m| m.id)
        .ok_or_else(|| StorageError::NotFound(SEED_DAPPS_DIR.to_string()))?;
    gdrive_delete_in(&client, token, name, &dapps).await
}

/// Автор: удаление legacy-файла из корня /ntfryseed на Google Drive.
pub async fn gdrive_delete_seed_root_file(token: &str, name: &str) -> Result<(), StorageError> {
    let client = http_client();
    let root = gdrive_seed_root(&client, token).await?;
    gdrive_delete_in(&client, token, name, &root).await
}

/// Автор: заливка данных в /ntfryseed/<subpath> на Google Drive.
/// Отсутствующие подпапки создаются; sha256 pre-check как у архивов.
pub async fn gdrive_upload_subpath(
    token: &str,
    subpath: &str,
    data: Vec<u8>,
    on_progress: Option<Box<dyn Fn(u64, u32) + Send + Sync + 'static>>,
) -> Result<bool, StorageError> {
    let parts = split_seed_subpath(subpath)?;
    let client = http_client();
    let mut parent = gdrive_ensure_folder(&client, token, SEED_DIR, "root").await?;
    for dir in &parts[..parts.len() - 1] {
        parent = gdrive_ensure_folder(&client, token, dir, &parent).await?;
    }
    let name = parts.last().expect("subpath не пуст").clone();
    gdrive_upsert_large(token, &name, &parent, data, on_progress).await
}

/// Автор: удаление произвольного subpath из /ntfryseed на Google Drive.
pub async fn gdrive_delete_subpath(token: &str, subpath: &str) -> Result<(), StorageError> {
    let parts = split_seed_subpath(subpath)?;
    let client = http_client();
    let mut parent = gdrive_seed_root(&client, token).await?;
    for dir in &parts[..parts.len() - 1] {
        parent = gdrive_find(&client, token, dir, &parent, "id")
            .await?
            .map(|m| m.id)
            .ok_or_else(|| StorageError::NotFound(dir.clone()))?;
    }
    gdrive_delete_in(&client, token, parts.last().expect("subpath не пуст"), &parent).await
}

/// Записи публичной папки из HTML embeddedfolderview: (id, имя).
/// Работает и для файлов, и для подпапок — id подпапки листается тем же путём.
fn gd_parse_entries(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("id=\"entry-") {
        rest = &rest[pos + 10..];
        let Some(end) = rest.find('"') else { break };
        let id = rest[..end].to_string();
        let Some(tpos) = rest.find("flip-entry-title\">") else { break };
        let tail = &rest[tpos + 18..];
        let Some(tend) = tail.find('<') else { break };
        out.push((id, tail[..tend].trim().to_string()));
        rest = tail;
    }
    out
}

/// Hidden-поля HTML-формы (антивирусный интерстишл Google при больших файлах).
fn gd_parse_hidden_inputs(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("<input type=\"hidden\"") {
        rest = &rest[pos + 20..];
        let Some(npos) = rest.find("name=\"") else { break };
        let nrest = &rest[npos + 6..];
        let Some(nend) = nrest.find('"') else { break };
        let name = nrest[..nend].to_string();
        let vrest = &nrest[nend..];
        let value = vrest
            .find("value=\"")
            .and_then(|vpos| {
                let v = &vrest[vpos + 7..];
                v.find('"').map(|vend| v[..vend].to_string())
            })
            .unwrap_or_default();
        out.push((name, value));
        rest = &nrest[nend..];
    }
    out
}

/// Анонимный листинг публичной папки Google Drive (без OAuth и API-ключа).
async fn gdrive_public_list(
    client: &reqwest::Client,
    folder_id: &str,
) -> Result<Vec<(String, String)>, StorageError> {
    let resp = client
        .get(GD_VIEW)
        .query(&[("id", folder_id)])
        .timeout(CTL_TIMEOUT)
        .send()
        .await?;
    if resp.status().as_u16() == 404 {
        return Err(StorageError::NotFound(format!("публичная папка {folder_id}")));
    }
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let html = resp.text().await?;
    Ok(gd_parse_entries(&html))
}

/// Скачивание публичного файла Google Drive по id (анонимно). При интерстишле
/// «файл слишком большой для антивирусной проверки» повторяет запрос
/// с hidden-полями формы download-form.
async fn gdrive_download_public(
    client: &reqwest::Client,
    id: &str,
    name_for_err: &str,
) -> Result<Vec<u8>, StorageError> {
    let resp = client
        .get(GD_DOWNLOAD)
        .query(&[("id", id), ("export", "download"), ("confirm", "t")])
        .send()
        .await?;
    if resp.status().as_u16() == 404 {
        return Err(StorageError::NotFound(name_for_err.to_string()));
    }
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let bytes = resp.bytes().await?.to_vec();
    if !(bytes.starts_with(b"<!DOCTYPE") || bytes.starts_with(b"<html")) {
        return Ok(bytes);
    }
    let html = String::from_utf8_lossy(&bytes);
    let params = gd_parse_hidden_inputs(&html);
    if params.is_empty() {
        return Err(StorageError::NotFound(format!(
            "{name_for_err}: Google вернул HTML вместо файла"
        )));
    }
    let resp = client.get(GD_DOWNLOAD).query(&params).send().await?;
    if !resp.status().is_success() {
        return Err(api_err(resp).await);
    }
    let bytes = resp.bytes().await?.to_vec();
    if bytes.starts_with(b"<!DOCTYPE") || bytes.starts_with(b"<html") {
        return Err(StorageError::NotFound(format!(
            "{name_for_err}: Google вернул HTML вместо файла"
        )));
    }
    Ok(bytes)
}

/// Пир: анонимно скачивает файл из чужой опубликованной папки Google Drive —
/// БЕЗ OAuth, только по ID папки. Путь с подпапками (dapps/…, users/…)
/// резолвится листингом каждого уровня.
pub async fn gdrive_fetch_public(folder_id: &str, file: &str) -> Result<Vec<u8>, StorageError> {
    let client = http_client();
    let mut current = folder_id.trim().to_string();
    for seg in file.split('/').filter(|s| !s.is_empty()) {
        let entries = gdrive_public_list(&client, &current).await?;
        match entries.into_iter().find(|(_, n)| n == seg) {
            Some((id, _)) => current = id,
            None => return Err(StorageError::NotFound(file.to_string())),
        }
    }
    gdrive_download_public(&client, &current, file).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::derive_address_keypair;

    const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    #[test]
    fn seeder_key_is_deterministic_and_pubkey_bound() {
        let (_, pk) = derive_address_keypair(MNEMONIC).unwrap();
        let a = derive_seeder_key(&pk);
        let b = derive_seeder_key(&pk);
        assert_eq!(a, b);
        let (_, pk2) = derive_address_keypair("другой секрет кошелька").unwrap();
        assert_ne!(a, derive_seeder_key(&pk2));
    }

    #[test]
    fn seal_open_roundtrip_and_tamper_detection() {
        let (sk, pk) = derive_address_keypair(MNEMONIC).unwrap();
        let pk_hex = hex::encode(pk);
        let payload = r#"{"v":1,"dapps":[{"id":"SXdX","name":"demo"}]}"#;
        let blob = seal(payload, &sk).unwrap();
        // чужой клиент читает только по pubkey
        assert_eq!(open(&blob, &pk_hex).unwrap(), payload);
        // подмена байтов облаком → AEAD-ошибка
        let mut broken = blob.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        assert!(open(&broken, &pk_hex).is_err());
        // чужой pubkey → другой ключ → не расшифруется
        let (_, pk2) = derive_address_keypair("другой секрет").unwrap();
        assert!(open(&blob, &hex::encode(pk2)).is_err());
    }

    #[test]
    fn signature_forgery_is_rejected() {
        let (sk_a, pk_a) = derive_address_keypair(MNEMONIC).unwrap();
        let (sk_b, _) = derive_address_keypair("злоумышленник").unwrap();
        // конверт подписан B, но зашифрован под ключ A (атака подмены автора)
        let payload = "{\"v\":1}";
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&sk_b).unwrap();
        let digest = Sha256::digest(payload.as_bytes());
        let msg = Message::from_digest_slice(&digest).unwrap();
        let sig = secp.sign_ecdsa(&msg, &sk);
        let env = Envelope {
            v: 1,
            pubkey: hex::encode(PublicKey::from_secret_key(&secp, &SecretKey::from_slice(&sk_a).unwrap()).serialize()),
            sig: hex::encode(sig.serialize_compact()),
            payload: payload.into(),
        };
        let plain = serde_json::to_vec(&env).unwrap();
        let key = derive_seeder_key(&pk_a);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let nonce = [7u8; 12];
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), plain.as_slice()).unwrap();
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        let err = open(&blob, &hex::encode(pk_a)).unwrap_err();
        assert!(err.to_string().contains("подпись"), "{err}");
    }

    #[test]
    fn uri_parse_roundtrip_with_base64_folder_token() {
        let (_, pk) = derive_address_keypair(MNEMONIC).unwrap();
        let pk_hex = hex::encode(pk);
        // реальный формат public_key Яндекса: base64 с / + =
        let token = "Y2lQjD3Aw5W8Hp/vSDcITT0mh+cvB3f8Sc1XXj8HWtWTgnFmiVDnPW1RtDVKGDrKac9kV6q9RNJZbz3xkzXBc0Q==";
        let uri = CloudSeederUri {
            provider: SeederProvider::Yandex,
            folder_token: token.into(),
            public_key: pk_hex.clone(),
        }
        .to_uri();
        assert!(uri.starts_with("sn://seed/y@"));
        let parsed = CloudSeederUri::parse(&uri).unwrap();
        assert_eq!(parsed.provider, SeederProvider::Yandex);
        assert_eq!(parsed.folder_token, token);
        assert_eq!(parsed.public_key, pk_hex);
        // мусор отвергается
        assert!(CloudSeederUri::parse("sth://seed/y@a@b").is_err());
        assert!(CloudSeederUri::parse("sn://seed/x@tok@00").is_err());
        assert!(CloudSeederUri::parse(&format!("sn://seed/y@@{pk_hex}")).is_err());
        assert!(CloudSeederUri::parse("sn://seed/y@tok@nothex").is_err());
        // Mail.ru: weblink содержит '/', pubkey всё равно отделяется справа
        let mr = CloudSeederUri {
            provider: SeederProvider::Mailru,
            folder_token: "6Piv/KCVSUz6BC".into(),
            public_key: pk_hex.clone(),
        }
        .to_uri();
        assert!(mr.starts_with("sn://seed/m@"));
        let parsed = CloudSeederUri::parse(&mr).unwrap();
        assert_eq!(parsed.provider, SeederProvider::Mailru);
        assert_eq!(parsed.folder_token, "6Piv/KCVSUz6BC");
        assert_eq!(parsed.public_key, pk_hex);
    }

    #[tokio::test]
    async fn resilient_put_streams_body_reports_progress_and_succeeds() {
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const TOTAL: usize = 1024 * 1024;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64 * 1024];
            let mut seen = 0usize;
            // выгребаем заголовки + всё тело, затем «сервер финализирует» → 201
            loop {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(700),
                    sock.read(&mut buf),
                )
                .await
                {
                    Ok(Ok(0)) => break,
                    Ok(Ok(n)) => seen += n,
                    _ => {
                        if seen > TOTAL {
                            break;
                        }
                    }
                }
            }
            sock.write_all(b"HTTP/1.1 201 Created\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let client = upload_client();
        let url = format!("http://{addr}/ntfryseed/dapps/x.enc");
        let progress = std::sync::Arc::new(AtomicU64::new(0));
        let p2 = progress.clone();
        let cb: Box<dyn Fn(u64, u32) + Send + Sync> =
            Box::new(move |sent, _| p2.store(sent, Ordering::Relaxed));
        resilient_put(|| client.put(&url), vec![7u8; TOTAL], Some(cb), |_strict| async {
            Ok::<bool, crate::error::StorageError>(false)
        })
        .await
        .unwrap();
        // колбэк прогресса дошёл до полного размера тела
        assert_eq!(progress.load(Ordering::Relaxed), TOTAL as u64);
    }

    #[tokio::test]
    async fn resilient_put_accepts_verified_file_without_reupload() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const TOTAL: usize = 256 * 1024;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let puts = std::sync::Arc::new(AtomicU32::new(0));
        let puts2 = puts.clone();
        // сервер принимает PUT целиком, но отвечает 503 (эмуляция «ответ потерян»)
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                puts2.fetch_add(1, Ordering::Relaxed);
                let mut buf = vec![0u8; 64 * 1024];
                let mut seen = 0usize;
                loop {
                    match tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        sock.read(&mut buf),
                    )
                    .await
                    {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => seen += n,
                        _ => {
                            if seen > TOTAL {
                                break;
                            }
                        }
                    }
                }
                let _ = sock
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n")
                    .await;
            }
        });
        let client = upload_client();
        let url = format!("http://{addr}/ntfryseed/dapps/x.enc");
        // verify: пре-чек (strict) — файла нет; после провала — «файл лёг» →
        // успех после ПЕРВОЙ попытки, признак uploaded=true
        let uploaded = resilient_put(|| client.put(&url), vec![7u8; TOTAL], None, |strict| async move {
            Ok::<bool, crate::error::StorageError>(!strict)
        })
        .await
        .unwrap();
        assert!(uploaded);
        assert_eq!(puts.load(Ordering::Relaxed), 1, "ре-заливки быть не должно");
    }

    #[tokio::test]
    async fn resilient_put_precheck_skips_upload_entirely() {
        // строгий пре-чек говорит «идентичный архив уже в облаке» →
        // ни одного сетевого PUT, возврат uploaded=false
        let client = upload_client();
        // адрес без слушателя: любая попытка PUT упала бы с connect error
        let uploaded = resilient_put(
            || client.put("http://127.0.0.1:1/ntfryseed/dapps/x.enc"),
            vec![7u8; 1024],
            None,
            |strict| async move { Ok::<bool, crate::error::StorageError>(strict) },
        )
        .await
        .unwrap();
        assert!(!uploaded, "заливка должна быть пропущена целиком");
    }

    #[test]
    fn seal_bytes_is_deterministic_for_same_content() {
        // фундамент пре-чека: неизменившийся архив → байт-в-байт тот же блоб
        let (sk, _) = derive_address_keypair(MNEMONIC).unwrap();
        let payload: Vec<u8> = (0..=255u8).cycle().take(8192).collect();
        assert_eq!(seal_bytes(&payload, &sk).unwrap(), seal_bytes(&payload, &sk).unwrap());
        // другое содержимое → другой nonce и другой блоб
        let mut other = payload.clone();
        other[0] ^= 1;
        assert_ne!(seal_bytes(&payload, &sk).unwrap(), seal_bytes(&other, &sk).unwrap());
    }

    #[test]
    fn gdrive_uri_parse_roundtrip() {
        let (_, pk) = derive_address_keypair(MNEMONIC).unwrap();
        let pk_hex = hex::encode(pk);
        let uri = CloudSeederUri {
            provider: SeederProvider::Google,
            folder_token: "1AbC_dEf-GhIjKlMnOpQrStUvWxYz1234".into(),
            public_key: pk_hex.clone(),
        }
        .to_uri();
        assert!(uri.starts_with("sn://seed/g@"));
        let parsed = CloudSeederUri::parse(&uri).unwrap();
        assert_eq!(parsed.provider, SeederProvider::Google);
        assert_eq!(parsed.folder_token, "1AbC_dEf-GhIjKlMnOpQrStUvWxYz1234");
        assert_eq!(parsed.public_key, pk_hex);
    }

    #[test]
    fn gdrive_parses_embeddedfolderview_entries() {
        let html = r#"<div class="flip-entry" id="entry-1FileIdAAA"><a class="flip-entry-borderless" href="https://drive.google.com/file/d/1FileIdAAA/view?usp=drive_web"><div class="flip-entry-info"><div class="flip-entry-title">dapps.enc</div></div></a></div>
<div class="flip-entry" id="entry-1FolderIdBBB"><a href="https://drive.google.com/embeddedfolderview?id=1FolderIdBBB"><div class="flip-entry-title">dapps</div></a></div>"#;
        let entries = gd_parse_entries(html);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ("1FileIdAAA".to_string(), "dapps.enc".to_string()));
        assert_eq!(entries[1], ("1FolderIdBBB".to_string(), "dapps".to_string()));
        assert!(gd_parse_entries("<html>no entries</html>").is_empty());
    }

    #[test]
    fn gdrive_parses_interstitial_hidden_inputs() {
        let html = r#"<form id="download-form" action="https://drive.usercontent.google.com/download" method="get">
<input type="hidden" name="id" value="1FileIdAAA">
<input type="hidden" name="export" value="download">
<input type="hidden" name="confirm" value="t">
<input type="hidden" name="uuid" value="abc-123"></form>"#;
        let params = gd_parse_hidden_inputs(html);
        assert_eq!(params.len(), 4);
        assert!(params.contains(&("uuid".to_string(), "abc-123".to_string())));
        assert!(params.contains(&("confirm".to_string(), "t".to_string())));
        assert!(gd_parse_hidden_inputs("<html>plain</html>").is_empty());
    }

    #[test]
    fn gdrive_query_name_is_escaped() {
        assert_eq!(gd_q_escape("it's"), "it\\'s");
        assert_eq!(gd_q_escape("plain.enc"), "plain.enc");
    }

    #[test]
    fn mailru_extract_weblink_get_url_from_html_state() {
        let html = r#"<script>window.cloud={"dispatcher":{"weblink_get":[{"count":1,"url":"https://cloclo1.datacloudmail.ru/weblink/get"}],"other":1}}</script>"#;
        assert_eq!(
            extract_url_after(html, "\"weblink_get\"").as_deref(),
            Some("https://cloclo1.datacloudmail.ru/weblink/get")
        );
        assert!(extract_url_after("no marker here", "\"weblink_get\"").is_none());
    }

    #[test]
    fn seal_open_bytes_roundtrip_and_tamper_detection() {
        let (sk, pk) = derive_address_keypair(MNEMONIC).unwrap();
        let pk_hex = hex::encode(pk);
        // «tar-архив»: произвольные бинарные байты, включая нули
        let payload: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let blob = seal_bytes(&payload, &sk).unwrap();
        assert_eq!(open_bytes(&blob, &pk_hex).unwrap(), payload);
        // подмена байтов облаком → AEAD-ошибка
        let mut broken = blob.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        assert!(open_bytes(&broken, &pk_hex).is_err());
        // чужой pubkey → другой ключ → не расшифруется
        let (_, pk2) = derive_address_keypair("другой секрет").unwrap();
        assert!(open_bytes(&blob, &hex::encode(pk2)).is_err());
        // текстовый конверт v1 не читается бинарным open_bytes
        let v1 = seal("{\"v\":1}", &sk).unwrap();
        assert!(open_bytes(&v1, &pk_hex).is_err());
        // v3 сжимает: повторяющийся payload заметно меньше исходника
        let texty = b"<html>same same same</html>".repeat(500);
        let packed_blob = seal_bytes(&texty, &sk).unwrap();
        assert!(packed_blob.len() < texty.len() / 2);
        assert_eq!(open_bytes(&packed_blob, &pk_hex).unwrap(), texty);
    }

    #[test]
    fn open_bytes_reads_legacy_v2_envelope() {
        // старые архивы в облаке (конверт v2, без сжатия) продолжают читаться
        use chacha20poly1305::aead::Aead;
        use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
        use rand::RngCore;
        let (sk, pk) = derive_address_keypair(MNEMONIC).unwrap();
        let pk_hex = hex::encode(pk);
        let payload: Vec<u8> = (0..=255u8).cycle().take(2048).collect();
        // реплика старого seal_bytes: [2] ‖ pubkey ‖ sig(raw payload) ‖ payload
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&sk).unwrap();
        let digest = Sha256::digest(&payload);
        let msg = Message::from_digest_slice(&digest).unwrap();
        let sig = secp.sign_ecdsa(&msg, &secret);
        let mut plain = vec![2u8];
        plain.extend_from_slice(&pk);
        plain.extend_from_slice(&sig.serialize_compact());
        plain.extend_from_slice(&payload);
        let key = derive_seeder_key(&pk);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), plain.as_slice()).unwrap();
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        assert_eq!(open_bytes(&blob, &pk_hex).unwrap(), payload);
    }
}
