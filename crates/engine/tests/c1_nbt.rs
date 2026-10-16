//! Bedrock NBT: byte-exact round trips, robustness, and the committed world fixture.

mod c1_common;
use beacon_engine::nbt::{self, Tag, parse_level_dat, write_level_dat};
use c1_common::*;
use proptest::prelude::*;

/// Run with `BEACON_REGEN_FIXTURE=1` to rewrite the committed fixture from the generator.
#[test]
fn committed_fixture_is_the_generators_output() {
    let p = fixtures().join("worlds/main/level.dat");
    if std::env::var_os("BEACON_REGEN_FIXTURE").is_some() {
        std::fs::write(&p, fixture_bytes()).unwrap();
    }
    assert_eq!(std::fs::read(&p).unwrap(), fixture_bytes());
}

#[test]
fn fixture_round_trips_byte_identically() {
    let bytes = std::fs::read(fixtures().join("worlds/main/level.dat")).unwrap();
    let (v, t) = parse_level_dat(&bytes).unwrap();
    assert_eq!(v, 10);
    assert_eq!(write_level_dat(v, &t), bytes);
    assert_eq!(
        nbt::get(&t, "LevelName"),
        Some(&Tag::string("Beacon Multipack"))
    );
}

/// `BEACON_LOCAL_LEVELDAT=a;b` (paths joined by `;`) round-trips real files; unset = skip.
#[test]
fn local_level_dats_round_trip() {
    let Some(v) = std::env::var_os("BEACON_LOCAL_LEVELDAT") else {
        return;
    };
    for f in v.to_string_lossy().split(';').filter(|s| !s.is_empty()) {
        let bytes = std::fs::read(f).unwrap();
        let (ver, t) = parse_level_dat(&bytes).unwrap_or_else(|e| panic!("{f}: {e}"));
        assert!(write_level_dat(ver, &t) == bytes, "{f}: not byte-identical");
        eprintln!("round-tripped {f} ({} bytes)", bytes.len());
    }
}

#[test]
fn every_tag_type_round_trips() {
    let all = Tag::Compound(vec![
        (b"b".to_vec(), Tag::Byte(-1)),
        (b"s".to_vec(), Tag::Short(-300)),
        (b"i".to_vec(), Tag::Int(i32::MIN)),
        (b"l".to_vec(), Tag::Long(i64::MAX)),
        // a signaling-NaN payload and negative zero must survive
        (b"f".to_vec(), Tag::Float(f32::from_bits(0x7fa0_0001))),
        (b"z".to_vec(), Tag::Float(-0.0)),
        (
            b"d".to_vec(),
            Tag::Double(f64::from_bits(0x7ff4_0000_0000_0001)),
        ),
        (b"ba".to_vec(), Tag::ByteArray(vec![0, 255, 7])),
        // not UTF-8, in a name and a value
        (vec![0xff, 0xfe], Tag::String(vec![0xc3, 0x28, 0x00])),
        (b"le".to_vec(), Tag::List(nbt::COMPOUND, vec![])),
        (b"le0".to_vec(), Tag::List(nbt::END, vec![])),
        (
            b"ll".to_vec(),
            Tag::List(
                nbt::LIST,
                vec![
                    Tag::List(nbt::INT, vec![Tag::Int(1)]),
                    Tag::List(nbt::BYTE, vec![]),
                ],
            ),
        ),
        // duplicate names keep their order
        (b"dup".to_vec(), Tag::Int(1)),
        (b"dup".to_vec(), Tag::Int(2)),
        (b"ia".to_vec(), Tag::IntArray(vec![1, -2])),
        (b"la".to_vec(), Tag::LongArray(vec![i64::MIN, 3])),
        (b"c".to_vec(), Tag::Compound(vec![])),
    ]);
    let bytes = write_level_dat(9, &all);
    let (v, back) = parse_level_dat(&bytes).unwrap();
    assert_eq!(v, 9);
    assert_eq!(write_level_dat(v, &back), bytes);
    // the header length is the payload length
    assert_eq!(
        i32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize,
        bytes.len() - 8
    );
}

#[test]
fn paths_get_and_set() {
    let mut t = Tag::Compound(vec![(b"a".to_vec(), Tag::Int(1))]);
    nbt::set(&mut t, "x.y.z", Tag::Byte(1)).unwrap();
    assert_eq!(nbt::get(&t, "x.y.z"), Some(&Tag::Byte(1)));
    assert_eq!(nbt::get(&t, "x.q"), None);
    assert!(
        nbt::set(&mut t, "a.b", Tag::Byte(1)).is_err(),
        "a is an Int"
    );
    assert!(nbt::set(&mut t, "x..y", Tag::Byte(1)).is_err());
}

#[test]
fn bad_input_is_an_error() {
    let ok = fixture_bytes();
    // header length lies
    let mut b = ok.clone();
    b[4] ^= 1;
    assert!(parse_level_dat(&b).is_err());
    // trailing garbage with a fixed-up length
    let mut b = ok.clone();
    b.push(0);
    let n = (b.len() - 8) as i32;
    b[4..8].copy_from_slice(&n.to_le_bytes());
    assert!(parse_level_dat(&b).is_err());
    assert!(parse_level_dat(&[]).is_err());
    // a 1 GiB-long byte array in a few bytes
    let mut p = vec![10, 0, 0, 7, 1, 0, b'x', 0xff, 0xff, 0xff, 0x3f];
    p.push(0);
    let mut b = vec![10, 0, 0, 0];
    b.extend((p.len() as i32).to_le_bytes());
    b.extend(p);
    assert!(parse_level_dat(&b).is_err());
    // nested far beyond the depth cap
    let mut p = vec![10, 0, 0];
    for _ in 0..100_000 {
        p.extend([10, 1, 0, b'a']);
    }
    let mut b = vec![10, 0, 0, 0];
    b.extend((p.len() as i32).to_le_bytes());
    b.extend(p);
    assert!(parse_level_dat(&b).is_err());
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut b = 10i32.to_le_bytes().to_vec();
    b.extend((payload.len() as i32).to_le_bytes());
    b.extend(payload);
    b
}

proptest! {
    /// Every strict prefix of a valid file, as is and re-framed, errors without panicking.
    #[test]
    fn truncation_is_an_error(cut in 0usize..10_000) {
        let ok = fixture_bytes();
        let cut = cut % ok.len();
        prop_assert!(parse_level_dat(&ok[..cut]).is_err());
        if cut >= 8 {
            prop_assert!(parse_level_dat(&framed(&ok[8..cut])).is_err());
        }
    }

    /// Flipped bytes of a valid file and pure garbage never panic.
    #[test]
    fn garbage_never_panics(
        flips in proptest::collection::vec((0usize..10_000, any::<u8>()), 1..8),
        junk in proptest::collection::vec(any::<u8>(), 0..300),
    ) {
        let mut b = fixture_bytes();
        for (i, v) in flips {
            let i = i % b.len();
            b[i] = v;
        }
        let _ = parse_level_dat(&b);
        let _ = parse_level_dat(&junk);
        let mut p = vec![10, 0, 0];
        p.extend(junk);
        let _ = parse_level_dat(&framed(&p));
    }
}
