use std::str::FromStr;

use polymarket_client_sdk::auth::LocalSigner;
use polymarket_client_sdk::types::Address;
use polymarket_client_sdk::{derive_proxy_wallet, derive_safe_wallet, POLYGON};

fn main() {
    let eoa: Address = Address::from_str("0xd23d2468bf08647c577b9674e6850c98f7d33bd8").unwrap();
    println!("EOA address: {eoa}");

    match derive_proxy_wallet(eoa, POLYGON) {
        Some(proxy) => println!("Derived PROXY wallet (sig_type=1): {proxy}"),
        None => println!("Proxy wallet derivation not supported"),
    }

    match derive_safe_wallet(eoa, POLYGON) {
        Some(safe) => println!("Derived GNOSIS SAFE wallet (sig_type=2): {safe}"),
        None => println!("Safe wallet derivation not supported"),
    }

    // Also try with the private key if available
    if let Ok(pk) = std::env::var("PRIVATE_KEY") {
        match LocalSigner::from_str(&pk) {
            Ok(signer) => {
                use polymarket_client_sdk::auth::Signer;
                let signer_addr = signer.address();
                println!("\nSigner address from PRIVATE_KEY: {signer_addr}");

                if signer_addr == eoa {
                    println!("CONFIRMED: PRIVATE_KEY matches EOA address");
                } else {
                    println!("WARNING: PRIVATE_KEY does NOT match EOA address!");
                    println!("  Expected: {eoa}");
                    println!("  Got:      {signer_addr}");

                    // Derive for the actual signer address too
                    match derive_proxy_wallet(signer_addr, POLYGON) {
                        Some(proxy) => println!("  Derived PROXY wallet (from pk): {proxy}"),
                        None => println!("  Proxy wallet derivation not supported"),
                    }
                    match derive_safe_wallet(signer_addr, POLYGON) {
                        Some(safe) => println!("  Derived GNOSIS SAFE wallet (from pk): {safe}"),
                        None => println!("  Safe wallet derivation not supported"),
                    }
                }
            }
            Err(e) => println!("Failed to parse PRIVATE_KEY: {e}"),
        }
    } else {
        println!("\nPRIVATE_KEY not set, skipping signer-based derivation");
    }
}
