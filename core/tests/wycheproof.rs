//! Wycheproof's ChaCha20-Poly1305 vectors (the negative-vectors gate): every valid case seals to the given bytes and opens;
//! every invalid case fails to open. A nonce of any length but 12 is refused by the type, which the file calls invalid.
#![warn(clippy::pedantic, clippy::nursery)]
use mudrpc_core::seal::{open, seal, Error};

fn hex(s: &str) -> Vec<u8> { if s == "-" { vec![] } else { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() } }

#[test]
fn every_wycheproof_chacha20_poly1305_case_gets_the_verdict_the_file_says() {
    let text = include_str!("wycheproof/chacha20_poly1305.txt");
    let (mut valid, mut invalid, mut other_nonce) = (0, 0, 0);
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(' ').collect();
        let (id, want) = (f[0], f[1] == "valid");
        let (key, iv, aad, msg, ct, tag) = (hex(f[2]), hex(f[3]), hex(f[4]), hex(f[5]), hex(f[6]), hex(f[7]));
        let key: [u8; 32] = key.try_into().unwrap();
        let Ok(nonce) = <[u8; 12]>::try_from(iv.as_slice()) else { assert!(!want, "case {id}: a nonce that is not 12 bytes cannot be valid"); other_nonce += 1; continue };
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
    assert_eq!((valid + invalid + other_nonce, valid > 200, invalid > 50), (325, true, true), "the whole file was read");
}
