//! Generates a bcrypt hash suitable for ATHENA_PASSWORD.
//!
//! Usage:
//!   cargo run --release --bin hash-password -- 'my secret password'
//!   cargo run --release --bin hash-password        (reads the password
//!                                                     interactively from stdin)
//!
//! Copy the printed `$2b$...` string into your `.env`:
//!   ATHENA_PASSWORD=$2b$10...
use std::io::{Read, Write};

fn main() {
    let password = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprint!("Passwort (Eingabe wird angezeigt): ");
            std::io::stderr().flush().ok();
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .expect("stdin konnte nicht gelesen werden");
            input.trim_end_matches(['\r', '\n']).to_string()
        }
    };

    if password.trim().is_empty() {
        eprintln!("Fehler: leeres Passwort.");
        std::process::exit(1);
    }

    // Cost 10 keeps verification comfortably under ~100ms even on a
    // Raspberry Pi 4; raise to 12 on faster hardware if desired.
    match bcrypt::hash(&password, 10) {
        Ok(hashed) => println!("{hashed}"),
        Err(e) => {
            eprintln!("Fehler beim Hashen: {e}");
            std::process::exit(1);
        }
    }
}
