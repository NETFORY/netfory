fn main() {
    let pubkey = std::env::args().nth(1).expect("pubkey hex");
    for f in ["dapps.enc", "nodes.enc", "seeders.enc", "vdapps.enc"] {
        let path = format!("/tmp/{f}");
        match std::fs::read(&path) {
            Ok(blob) => match ntfry::seeder::open(&blob, &pubkey) {
                Ok(payload) => println!("=== {f} ({} bytes payload) ===\n{}\n", payload.len(), &payload[..payload.len().min(1500)]),
                Err(e) => println!("=== {f} DECRYPT ERROR: {e:?} ==="),
            },
            Err(e) => println!("=== {f} READ ERROR: {e} ==="),
        }
    }
}
