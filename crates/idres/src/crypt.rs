//! Decryption for `binaryFile` resources (`*.bfile`), keyed by the resource's short name.
//! Scheme documented by emoose (DOOMExtract/idCrypt):
//! `salt[0xC] | iv[0x10] | AES-128-CBC data | HMAC-SHA256[0x20]`, key = SHA256(salt + "swapTeam\n\0" + name).

use aes::Aes128;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const HEADER: usize = 0xC + 0x10;
const MAC: usize = 0x20;

pub fn decrypt(input: &[u8], key: &str) -> Option<Vec<u8>> {
    if input.len() < HEADER + MAC {
        return None;
    }
    let (signed, mac) = input.split_at(input.len() - MAC);
    let salt = &signed[..0xC];
    let iv = &signed[0xC..HEADER];
    let data = &signed[HEADER..];

    let mut sha = Sha256::new();
    sha.update(salt);
    sha.update(b"swapTeam\n\0");
    sha.update(key.as_bytes());
    let digest = sha.finalize();

    let mut hmac = <Hmac<Sha256> as Mac>::new_from_slice(&digest).ok()?;
    hmac.update(signed);
    hmac.verify_slice(mac).ok()?;

    cbc::Decryptor::<Aes128>::new_from_slices(&digest[..16], iv)
        .ok()?
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .ok()
}
