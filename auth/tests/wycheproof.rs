//! Wycheproof's Ed25519 vectors (the negative-vectors gate): every valid signature verifies, every invalid one is refused,
//! including non-canonical S, wrong lengths, and signatures over the wrong message. A signature of any length but 64 is
//! refused by the type, which the file calls invalid.
#![warn(clippy::pedantic, clippy::nursery)]
use mudrpc_auth::ed25519::verify;

fn hex(s: &str) -> Vec<u8> { if s == "-" { vec![] } else { (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() } }

#[test]
fn every_wycheproof_ed25519_case_gets_the_verdict_the_file_says() {
    let text = include_str!("wycheproof/ed25519.txt");
    let (mut valid, mut invalid) = (0, 0);
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(' ').collect();
        let (id, want) = (f[0], f[1] == "valid");
        let (pk, msg, sig) = (hex(f[2]), hex(f[3]), hex(f[4]));
        let pk: [u8; 32] = pk.try_into().unwrap();
        let got = <[u8; 64]>::try_from(sig.as_slice()).is_ok_and(|sig| verify(&pk, &[&msg], &sig));
        assert_eq!(got, want, "case {id}");
        if want { valid += 1 } else { invalid += 1 }
    }
    assert_eq!((valid + invalid, valid > 50, invalid > 50), (151, true, true), "the whole file was read");
}
