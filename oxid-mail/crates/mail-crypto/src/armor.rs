// ASCII-armor NTFRY-payload для тела письма + детект/извлечение.
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

pub const MARK_START: &str = "-----NTFRY START-----";
pub const MARK_END: &str = "-----NTFRY END-----";

/// Человекочитаемая шапка для обычных клиентов + armored payload.
pub fn armor_body(payload: &[u8]) -> String {
    let b64 = B64.encode(payload);
    let wrapped: Vec<&str> = b64
        .as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    format!(
        "This message is encrypted with SmartNet Oxid Mail.\n\
         Open it in the NETFORY 0xID mail: https://smartholdem.io\n\n\
         {MARK_START}\n{}\n{MARK_END}\n",
        wrapped.join("\n")
    )
}

pub fn contains_ntfry(body: &str) -> bool {
    body.contains(MARK_START) && body.contains(MARK_END)
}

pub fn extract(body: &str) -> Option<Vec<u8>> {
    let start = body.find(MARK_START)? + MARK_START.len();
    let end = body[start..].find(MARK_END)? + start;
    let b64: String = body[start..end].chars().filter(|c| !c.is_whitespace()).collect();
    B64.decode(b64).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armor_roundtrip() {
        let payload: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let body = armor_body(&payload);
        assert!(contains_ntfry(&body));
        assert_eq!(extract(&body).unwrap(), payload);
        assert!(!contains_ntfry("обычное письмо"));
        assert!(extract("обычное письмо").is_none());
    }
}
