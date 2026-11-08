//! Wycheproof's AES-GCM vectors for 256-bit keys, 12-byte IVs and 16-byte tags (the negative-vectors gate): every valid
//! case seals to the given bytes and opens; every invalid case fails to open.
#![warn(clippy::pedantic, clippy::nursery)]
use mudrpc_core::seal::Error;
use mudrpc_gcm::{open, seal};

fn hex(s: &str) -> Vec<u8> { if s == "-" { vec![] } else { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() } }

#[test]
fn every_wycheproof_aes_gcm_case_gets_the_verdict_the_file_says() {
    let text = include_str!("wycheproof/aes_gcm.txt");
    let (mut valid, mut invalid) = (0, 0);
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(' ').collect();
        let (id, want) = (f[0], f[1] == "valid");
        let (key, iv, aad, msg, ct, tag) = (hex(f[2]), hex(f[3]), hex(f[4]), hex(f[5]), hex(f[6]), hex(f[7]));
        let (key, nonce): ([u8; 32], [u8; 12]) = (key.try_into().unwrap(), iv.try_into().unwrap());
        let mut wire = vec![0u8; 12 + msg.len() + 16];
        if want {
            let n = seal(&key, &nonce, &aad, &msg, &mut wire).unwrap();
            assert_eq!((&wire[12..n - 16], &wire[n - 16..n]), (ct.as_slice(), tag.as_slice()), "case {id}: seal");
        }
        let mut given = nonce.to_vec();
        given.extend_from_slice(&ct);
        given.extend_from_slice(&tag);
        let mut out = vec![0u8; ct.len()];
        match open(&key, &aad, &given, &mut out) {
            Ok(n) => { assert!(want, "case {id}: opened an invalid case"); assert_eq!(&out[..n], msg.as_slice(), "case {id}"); valid += 1; }
            Err(e) => { assert!(!want, "case {id}: refused a valid case: {e:?}"); assert!(matches!(e, Error::Tag | Error::Short), "case {id}: {e:?}"); invalid += 1; }
        }
    }
    assert_eq!((valid, invalid), (39, 27), "the whole file was read");
}
