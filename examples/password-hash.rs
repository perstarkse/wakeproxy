use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHasher, SaltString};

fn main() -> anyhow::Result<()> {
    let password = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: cargo run --example password-hash -- <password>"))?;

    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
        .to_string();

    println!("{hash}");
    Ok(())
}
