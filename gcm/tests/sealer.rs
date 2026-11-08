//! The `Gcm` sealer: `Key`'s three slots and rotation, the header masked by AES-ECB of the sixteen bytes after it under a
//! derived header key, version 3 on the wire, so a mixed fleet is two `Version(n)` counts.
#![warn(clippy::pedantic, clippy::nursery)]
#![allow(clippy::many_single_char_names)] // test scripts: `a` and `b` are the sealers, `w` a wire, `n` its length
use mudrpc_core::rpc::{Dropped, Incoming, Outgoing, Peer};
use mudrpc_core::seal::{self, Key, Sealer, Slot};
use mudrpc_gcm::{aes, check, open, Gcm};

#[test]
fn the_gcm_sealer_masks_the_header_rotates_through_three_slots_and_reports_which_opened() {
    let (k1, k2, k3) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    let mut a = Gcm::new(k1);
    let nonce = [0x11u8, 0x22, 0x33, 0x44, 0, 0, 0, 7, 0, 0, 0, 9];
    let (mut room, mut w) = (*b"waypoint and room", [0u8; 64]);
    let n = a.seal(&nonce, b"nav", &mut room, 8, &mut w).unwrap();
    assert_eq!((n, room), (12 + 8 + 16, *b"waypoint and room"), "a trailer of 0: the room after the message is neither sealed nor touched");
    assert_eq!(a.seal(&nonce, b"nav", &mut room, 18, &mut [0u8; 64]), Err(seal::Error::Full), "n past the buffer");
    let mut hk = [[0xffu8; 16], [0xff; 16]];
    hk[1][0] = 0xfe;
    for b in &mut hk { b[12..].fill(0); }
    aes::encrypt(&aes::schedule(&k1), &mut hk); // the header key: two blocks GCM never encrypts under k1
    let hk = aes::schedule(hk.as_flattened().try_into().unwrap());
    let mut e = [[0u8; 16]; 2];
    e[0].copy_from_slice(&w[12..28]);
    aes::encrypt(&hk, &mut e);
    let header: Vec<u8> = nonce.iter().zip(&e[0]).map(|(n, m)| n ^ m).collect();
    assert_eq!(&w[..12], header, "the header is the nonce under AES-ECB, under the header key, of the sixteen bytes after it");
    let mut bare = w;
    bare[..12].copy_from_slice(&nonce);
    let mut out = [0u8; 64];
    assert_eq!(open(&k1, b"nav", &bare[..n], &mut out), Ok(8), "beneath the mask, AES-256-GCM as SP 800-38D has it");
    let (mut z, mut h) = ([[0u8; 16]; 2], [[0u8; 16]; 2]);
    aes::encrypt(&hk, &mut z);
    aes::encrypt(&aes::schedule(&k1), &mut h);
    assert_eq!((check(&k1)[..] == z[0][..12], check(&k1)[..] == h[0][..12]), (true, false), "the checksum is the mask of a zero sample, and no part of H");
    let o = a.open(b"nav", &w[..n], &mut out).unwrap();
    assert_eq!((o.len, o.nonce, o.slot, &out[..8]), (8, nonce, Slot::Current, &b"waypoint"[..]));
    assert_eq!(a.open(b"other", &w[..n], &mut out), Err(seal::Error::Tag));
    let mut b = Gcm::new(k2);
    assert_eq!(b.open(b"nav", &w[..n], &mut out), Err(seal::Error::Tag), "a key never held");
    b.stage(k1);
    assert_eq!(b.open(b"nav", &w[..n], &mut out).map(|o| o.slot), Ok(Slot::Next), "a unit that has not rotated yet opens the newcomer's wire");
    a.stage(k2);
    a.rotate();
    assert_eq!(a.open(b"nav", &w[..n], &mut out).map(|o| o.slot), Ok(Slot::Previous), "after rotation the old key is previous");
    let mut w2 = [0u8; 64];
    let n2 = a.seal(&nonce, b"nav", &mut b"again".to_owned(), 5, &mut w2).unwrap();
    assert_ne!(&w2[..12], &w[..12], "a new key, a new mask");
    assert_eq!(Gcm::new(k2).open(b"nav", &w2[..n2], &mut out).map(|o| o.slot), Ok(Slot::Current));
    a.retire();
    assert_eq!(a.open(b"nav", &w[..n], &mut out), Err(seal::Error::Tag), "retired: the old key opens nothing");
    a.stage(k3);
    a.rotate();
    assert_eq!(a.open(b"nav", &w2[..n2], &mut out).map(|o| o.slot), Ok(Slot::Previous));
    assert_eq!(a.open(b"nav", &w[..27], &mut out), Err(seal::Error::Short));
    assert_eq!(a.open(b"nav", &w2[..n2], &mut [0u8; 2]), Err(seal::Error::Full), "an output buffer too small is Full, under every slot");
    assert!(a.intact() && Gcm::new(k3).intact());
}

#[test]
fn the_gcm_header_mask_is_per_wire_so_nothing_of_the_nonce_reaches_the_air() {
    let mut k = Gcm::new([5u8; 32]);
    let mut headers = Vec::new();
    for counter in 0..1000u32 {
        let mut nonce = [0u8; 12];
        nonce[0] = 7; // sender 7, epoch 0: the high bytes are zero, as in every real fleet
        nonce[8..].copy_from_slice(&counter.to_le_bytes());
        let mut w = [0u8; 64];
        k.seal(&nonce, b"nav", &mut b"same payload".to_owned(), 12, &mut w).unwrap();
        assert_eq!(k.open(b"nav", &w[..40], &mut [0u8; 64]).map(|o| o.nonce), Ok(nonce), "the receiver has the sample before it needs the mask");
        headers.push(w[..12].to_vec());
    }
    for i in 0..12 { assert!(headers.iter().any(|h| h[i] != headers[0][i]), "header byte {i} is constant: a pad reused over a zero"); }
}

#[test]
fn a_mixed_fleet_is_two_version_counts_never_a_tag_failure() {
    fn wire<S: Sealer>(p: &mut Peer<S, 256, 4, 8, 16, 4>) -> (String, Vec<u8>) {
        p.publish(1, "nav", b"hello", 60).unwrap();
        let mut v = Vec::new();
        while let Some(o) = p.outgoing(1) {
            if let Outgoing::Send { topic, wire, .. } = o { v.push((topic.to_string(), wire.to_vec())); }
            p.sent();
        }
        v.remove(0)
    }
    let mut chacha = Peer::<Key, 256, 4, 8, 16, 4>::new("gcs", Key::new([7; 32]), 1, 1).unwrap();
    let (mut a, mut b) = (Peer::<Gcm, 256, 4, 8, 16, 4>::new("a", Gcm::new([7; 32]), 2, 1).unwrap(), Peer::<Gcm, 256, 4, 8, 16, 4>::new("b", Gcm::new([7; 32]), 3, 1).unwrap());
    for p in [&mut a, &mut b] { p.listen("nav").unwrap(); }
    chacha.listen("nav").unwrap();
    let (c, g) = (wire(&mut chacha), wire(&mut a));
    assert_eq!((c.1[0], g.1[0], c.1.len()), (1, 3, g.1.len()), "the version byte differs and nothing else in the wire's shape");
    assert!(matches!(a.deliver(1, &c.0, &c.1, &mut [0u8; 256]), Incoming::Dropped(Dropped::Version(1))));
    assert!(matches!(chacha.deliver(1, &g.0, &g.1, &mut [0u8; 256]), Incoming::Dropped(Dropped::Version(3))));
    assert!(matches!(b.deliver(1, &g.0, &g.1, &mut [0u8; 256]), Incoming::Event { bytes: b"hello", .. }), "and the AES peers hear each other");
}
