// Утилита e2e-теста: печатает armored NTFRY-тело письма для seed+email.
// cargo run -p mail-crypto --example mkenv -- "<seed>" "<email>" "<text>"
use mail_crypto::{armor, envelope, hash, keys};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (seed, email, text) = (&args[1], &args[2], &args[3]);
    let master = keys::master_from_secret(seed);
    let h32 = hash::email_h32(email);
    let rcpt = envelope::RecipientKey {
        hint: hash::rcpt_hint(&h32),
        x25519: keys::x25519_public(&keys::derive_x25519(&master)),
    };
    let content = envelope::EnvelopeContent {
        subject: "Тайная тема e2e".into(),
        text: text.clone(),
        html: None,
        attachments: vec![],
        ts: 1_760_000_000,
        in_reply_to: None,
        reply_x25519_hex: None,
        reply_iroh_hex: None,
    };
    let payload = envelope::seal(&content, &master, &[rcpt]).expect("seal");
    println!("{}", armor::armor_body(&payload));
}
