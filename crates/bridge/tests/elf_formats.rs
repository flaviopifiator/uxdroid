//! The pure decoders handle untrusted bytes: valid data decodes exactly, anything else is an error.
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use uxdroid_bridge::elf::{
    ElfError, Rela, for_each_packed, for_each_relr, gnu_hash, parse_dynamic, parse_header,
    parse_program_headers, sysv_hash,
};

const R_RELATIVE: u64 = 8;

fn decode_packed(data: &[u8], limit: u64) -> Result<Vec<Rela>, ElfError> {
    let mut relocations = Vec::new();
    for_each_packed(data, limit, |rela| {
        relocations.push(rela);
        Ok(())
    })?;
    Ok(relocations)
}

fn sleb(mut value: i64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            return;
        }
    }
}

fn packed(values: &[i64]) -> Vec<u8> {
    let mut data = b"APS2".to_vec();
    for &value in values {
        sleb(value, &mut data);
    }
    data
}

#[test]
fn packed_relocations_decode_grouped_values() {
    const GROUPED_BY_INFO: i64 = 1;
    const GROUPED_BY_OFFSET_DELTA: i64 = 2;
    const GROUP_HAS_ADDEND: i64 = 8;
    let flags = GROUPED_BY_INFO | GROUPED_BY_OFFSET_DELTA | GROUP_HAS_ADDEND;
    // Two relocations, starting at 0x1000: one group sharing info and stride, per-entry addends.
    let data = packed(&[2, 0x1000, 2, flags, 8, R_RELATIVE as i64, 0x10, -4]);

    let decoded = decode_packed(&data, 100).unwrap();

    assert_eq!(
        decoded,
        vec![
            Rela {
                offset: 0x1008,
                kind: 8,
                symbol: 0,
                addend: 0x10
            },
            Rela {
                offset: 0x1010,
                kind: 8,
                symbol: 0,
                addend: 0xC
            },
        ]
    );
}

#[test]
fn packed_relocations_decode_ungrouped_values_and_symbols() {
    let info = (3i64 << 32) | 6;
    let data = packed(&[1, 0x2000, 1, 0, 0x40, info]);

    let decoded = decode_packed(&data, 100).unwrap();

    assert_eq!(
        decoded,
        vec![Rela {
            offset: 0x2040,
            kind: 6,
            symbol: 3,
            addend: 0
        }]
    );
}

#[test]
fn packed_relocations_reject_damage_without_panicking() {
    assert!(decode_packed(b"", 100).is_err());
    assert!(decode_packed(b"APS1", 100).is_err());
    assert!(decode_packed(&packed(&[-1, 0]), 100).is_err());
    // More relocations than the image could hold: a few bytes must not cost endless work.
    assert!(decode_packed(&packed(&[i64::MAX, 0, 1, 3, 0, 8]), 100).is_err());
    assert!(decode_packed(&packed(&[2, 0, 5, 3, 0, 8]), 100).is_err());
    assert!(decode_packed(&packed(&[2, 0, 0, 3]), 100).is_err());
    assert!(decode_packed(&[b'A', b'P', b'S', b'2', 0xFF, 0xFF, 0xFF], 100).is_err());
    assert!(decode_packed(&[b"APS2".as_slice(), &[0x80; 32]].concat(), 100).is_err());

    let valid = packed(&[2, 0x1000, 2, 11, 8, 8, 16, -4]);
    for length in 0..valid.len() {
        let _ = decode_packed(&valid[..length], 100);
    }
}

fn relr_addresses(entries: &[u64]) -> Vec<u64> {
    let table: Vec<u8> = entries
        .iter()
        .flat_map(|entry| entry.to_le_bytes())
        .collect();
    let mut addresses = Vec::new();
    for_each_relr(&table, |address| {
        addresses.push(address);
        Ok(())
    })
    .unwrap();
    addresses
}

#[test]
fn relr_decodes_addresses_and_bitmaps() {
    // An address, then a bitmap whose bits 1 and 3 select the 1st and 3rd following words,
    // then a bitmap continuing 63 words later.
    let bitmap = (1 << 1) | (1 << 3) | 1;
    let next_bitmap = (1 << 1) | 1;

    assert_eq!(
        relr_addresses(&[0x1000, bitmap, next_bitmap]),
        vec![0x1000, 0x1008, 0x1018, 0x1008 + 63 * 8]
    );
}

#[test]
fn relr_rejects_overflowing_addresses() {
    let table: Vec<u8> = [u64::MAX - 7, 0b11]
        .iter()
        .flat_map(|entry: &u64| entry.to_le_bytes())
        .collect();
    assert!(for_each_relr(&table, |_| Ok(())).is_err());
}

#[test]
fn symbol_hashes_match_the_elf_specification() {
    assert_eq!(gnu_hash(b""), 5381);
    assert_eq!(gnu_hash(b"printf"), 0x156B_2BB8);
    assert_eq!(sysv_hash(b"printf"), 0x0779_05A6);
}

fn valid_header() -> [u8; 64] {
    let mut header = [0u8; 64];
    header[..4].copy_from_slice(b"\x7fELF");
    header[4] = 2;
    header[5] = 1;
    header[6] = 1;
    header[16] = 3;
    header[18] = 62;
    header[32] = 64;
    header[54] = 56;
    header[56] = 1;
    header
}

#[test]
fn elf_headers_are_validated() {
    assert_eq!(parse_header(&valid_header()).unwrap().count, 1);
    assert!(parse_header(&valid_header()[..63]).is_err());

    for (offset, value, message) in [
        (0, 0u8, "bad magic"),
        (4, 1, "ELF64"),
        (16, 2, "shared object"),
        (18, 183, "x86_64"),
        (54, 32, "program header size"),
    ] {
        let mut header = valid_header();
        header[offset] = value;
        let error = parse_header(&header).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
    }
}

fn load_header(vaddr: u64, offset: u64, filesz: u64, memsz: u64, flags: u32) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend_from_slice(&1u32.to_le_bytes());
    raw.extend_from_slice(&flags.to_le_bytes());
    raw.extend_from_slice(&offset.to_le_bytes());
    raw.extend_from_slice(&vaddr.to_le_bytes());
    raw.extend_from_slice(&vaddr.to_le_bytes());
    raw.extend_from_slice(&filesz.to_le_bytes());
    raw.extend_from_slice(&memsz.to_le_bytes());
    raw.extend_from_slice(&4096u64.to_le_bytes());
    raw
}

#[test]
fn program_headers_reject_unsafe_mappings() {
    let good = load_header(0x1000, 0x1000, 16, 32, 5);
    assert_eq!(parse_program_headers(&good).unwrap().len(), 1);

    for (raw, message) in [
        (Vec::new(), "no loadable"),
        (load_header(0, 0, 32, 16, 4), "larger on disk"),
        (load_header(u64::MAX - 4, 0, 0, 16, 4), "overflows"),
        (load_header(0, u64::MAX, 16, 16, 4), "overflows"),
        (load_header(0x1000, 0x1234, 16, 16, 4), "not congruent"),
        (load_header(0, 0, 16, 16, 7), "writable and executable"),
        (
            [
                load_header(0x2000, 0, 16, 16, 4),
                load_header(0x1000, 0, 16, 16, 4),
            ]
            .concat(),
            "overlapping",
        ),
    ] {
        let error = parse_program_headers(&raw).unwrap_err().to_string();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn dynamic_sections_reject_what_the_loader_cannot_honor() {
    let entries = |pairs: &[(u64, u64)]| -> Vec<u8> {
        pairs
            .iter()
            .flat_map(|(tag, value)| [tag.to_le_bytes(), value.to_le_bytes()].concat())
            .collect()
    };

    let dynamic = parse_dynamic(&entries(&[(1, 5), (1, 9), (5, 0x100), (0, 0)])).unwrap();
    assert_eq!(dynamic.needed, vec![5, 9]);
    assert_eq!(dynamic.strtab, 0x100);

    let too_many: Vec<(u64, u64)> = (0..257).map(|_| (1, 0)).chain([(0, 0)]).collect();
    assert!(parse_dynamic(&entries(&too_many)).is_err());

    assert!(parse_dynamic(&entries(&[(5, 0x100)])).is_err());
    assert!(parse_dynamic(&entries(&[(22, 0), (0, 0)])).is_err());
    assert!(parse_dynamic(&entries(&[(30, 4), (0, 0)])).is_err());
    assert!(parse_dynamic(&entries(&[(20, 17), (0, 0)])).is_err());
}

#[test]
fn a_rejected_packed_relocation_stops_a_huge_declared_count_at_once() {
    let bomb = packed(&[i64::from(i32::MAX), 0, i64::from(i32::MAX), 3, 8, 8]);
    let mut applied = 0;

    let result = for_each_packed(&bomb, u64::MAX, |_| {
        applied += 1;
        Err(ElfError::Malformed("outside the image"))
    });

    assert!(result.is_err());
    assert_eq!(applied, 1);
}
