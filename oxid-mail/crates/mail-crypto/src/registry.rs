// Кодирование/парсинг записей реестра в vendorField (лимит 255 символов).
// oxid1:<H16>:<X25519>:<IrohNodeId>  |  oxid1:revoke:<H16>   (base64url без padding)
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;

pub const PREFIX: &str = "oxid1";
pub const REGISTRY_ADDRESS: &str = "SMaiLvLLdWmZ5aRpHPtDhciaBCJUgHESHD";
/// Amount и Fee транзакции привязки, в arktoshi (0.5 STH при 8 знаках).
pub const BIND_AMOUNT: u64 = 50_000_000;
pub const BIND_FEE: u64 = 50_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindRecord {
    pub h16: [u8; 16],
    pub x25519: [u8; 32],
    pub iroh_node: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryRecord {
    Bind(BindRecord),
    Revoke([u8; 16]),
}

pub fn encode_bind(r: &BindRecord) -> String {
    format!(
        "{PREFIX}:{}:{}:{}",
        B64.encode(r.h16),
        B64.encode(r.x25519),
        B64.encode(r.iroh_node)
    )
}

pub fn encode_revoke(h16: &[u8; 16]) -> String {
    format!("{PREFIX}:revoke:{}", B64.encode(h16))
}

fn dec<const N: usize>(s: &str) -> Option<[u8; N]> {
    let v = B64.decode(s).ok()?;
    v.try_into().ok()
}

pub fn parse(vendor: &str) -> Option<RegistryRecord> {
    let v = vendor.trim();
    let rest = v.strip_prefix("oxid1:")?;
    if let Some(h) = rest.strip_prefix("revoke:") {
        return Some(RegistryRecord::Revoke(dec::<16>(h)?));
    }
    let mut it = rest.split(':');
    let (a, b, c) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() {
        return None;
    }
    Some(RegistryRecord::Bind(BindRecord {
        h16: dec::<16>(a)?,
        x25519: dec::<32>(b)?,
        iroh_node: dec::<32>(c)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_length() {
        let r = BindRecord { h16: [1; 16], x25519: [2; 32], iroh_node: [3; 32] };
        let s = encode_bind(&r);
        assert!(s.len() <= 255, "vendorField overflow: {}", s.len());
        assert_eq!(s.len(), 116);
        assert_eq!(parse(&s), Some(RegistryRecord::Bind(r.clone())));
        let rv = encode_revoke(&r.h16);
        assert_eq!(parse(&rv), Some(RegistryRecord::Revoke([1; 16])));
    }

    #[test]
    fn garbage_rejected() {
        assert_eq!(parse("smartnet:1.2.3:abc"), None);
        assert_eq!(parse("oxid1:short"), None);
        assert_eq!(parse("oxid1:a:b:c:d"), None);
        assert_eq!(parse(""), None);
    }
}
