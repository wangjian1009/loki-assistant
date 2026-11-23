use std::{fs::File, io::BufReader, path::Path};

use pgp::{
    composed::{DecryptionOptions, Message, PlainSessionKey, RawSessionKey, TheRing},
    crypto::sym::SymmetricKeyAlgorithm,
};

pub(super) fn decrypt(file: &Path, algorithm: u8, bytes: &[u8]) -> pgp::errors::Result<String> {
    let open = || -> pgp::errors::Result<Message<'static>> {
        Message::from_reader(BufReader::new(File::open(file)?)).map(|(message, _)| message)
    };
    let decrypt_with = |key| {
        let ring = TheRing {
            session_keys: vec![key],
            decrypt_options: DecryptionOptions::new().enable_gnupg_aead(),
            ..Default::default()
        };
        open()?
            .decrypt_the_ring(ring, true)
            .map(|(message, _)| message)
    };
    let mut message = decrypt_with(PlainSessionKey::V3_4 {
        sym_alg: SymmetricKeyAlgorithm::from(algorithm),
        key: RawSessionKey::from(bytes),
    })
    .or_else(|_| {
        decrypt_with(PlainSessionKey::V6 {
            key: RawSessionKey::from(bytes),
        })
    })?;
    while message.is_compressed() {
        message = message.decompress()?;
    }
    Ok(message.as_data_string()?)
}
