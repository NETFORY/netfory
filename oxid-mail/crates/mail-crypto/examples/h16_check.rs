// Диагностика: h16 для email vs on-chain запись
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;

fn main() {
    for email in std::env::args().skip(1) {
        let h32 = mail_crypto::hash::email_h32(&email);
        let h16 = mail_crypto::hash::h16(&h32);
        println!("{email} -> h16 b64url: {}", B64.encode(h16));
    }
}
