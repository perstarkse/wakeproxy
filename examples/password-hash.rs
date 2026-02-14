use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHasher, SaltString};
use thiserror::Error;

#[derive(Debug, Error)]
enum ExampleError {
    #[error("usage: cargo run --example password-hash -- <password>")]
    MissingPassword,
    #[error("failed to hash password")]
    Hash(String),
}

fn main() -> Result<(), ExampleError> {
    let password = std::env::args()
        .nth(1)
        .ok_or(ExampleError::MissingPassword)?;

    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|error| ExampleError::Hash(error.to_string()))?
        .to_string();

    println!("{hash}");
    Ok(())
}
