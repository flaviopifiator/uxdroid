//! Byte layouts from AOSP's `frameworks/native/libs/binder/Parcel.cpp` (Android 16, Apache-2.0),
//! for a kernel-binder parcel on a little-endian host.

use super::*;

type P = Parcel<&'static str>;

fn bytes(parcel: &P) -> Vec<u8> {
    parcel.data()[..parcel.data_size()].to_vec()
}

#[test]
fn integers_are_little_endian_and_four_byte_aligned() {
    let mut p = P::new();
    p.write_i32(0x0102_0304).unwrap();
    p.write_i64(-2).unwrap();
    p.write_f32(1.5).unwrap();
    p.write_f64(-0.25).unwrap();
    let mut expected = vec![4, 3, 2, 1];
    expected.extend((-2i64).to_le_bytes());
    expected.extend(1.5f32.to_le_bytes());
    expected.extend((-0.25f64).to_le_bytes());
    assert_eq!(bytes(&p), expected);
    assert_eq!((p.data_size(), p.data_position()), (24, 24));

    p.set_data_position(0).unwrap();
    assert_eq!(p.read_i32(), Ok(0x0102_0304));
    assert_eq!(p.read_i64(), Ok(-2));
    assert_eq!(p.read_f32(), Ok(1.5));
    assert_eq!(p.read_f64(), Ok(-0.25));
    assert_eq!(p.data_avail(), 0);
}

#[test]
fn a_read_past_the_end_fails_without_moving() {
    let mut p = P::new();
    p.write_i32(7).unwrap();
    p.set_data_position(2).unwrap();
    assert_eq!(p.read_i32(), Err(NOT_ENOUGH_DATA));
    assert_eq!(p.data_position(), 2);
    p.set_data_position(0).unwrap();
    assert_eq!(p.read_i64(), Err(NOT_ENOUGH_DATA));
}

#[test]
fn growth_follows_parcel_cpp() {
    // growData: max(((size + len) * 3) / 2, 128).
    let mut p = P::new();
    p.write_i32(1).unwrap();
    assert_eq!(p.data_capacity(), 128);
    for _ in 0..31 {
        p.write_i32(1).unwrap();
    }
    assert_eq!(p.data_capacity(), 128);
    p.write_i32(1).unwrap();
    assert_eq!(p.data_capacity(), (128 + 4) * 3 / 2);
}

#[test]
fn data_size_counts_the_position_past_the_end() {
    let mut p = P::new();
    p.write_i32(1).unwrap();
    p.set_data_position(100).unwrap();
    assert_eq!(p.data_size(), 100);
    assert_eq!(p.data_avail(), 0);
    // growData refuses to grow from a position past the end.
    p.set_data_position(200).unwrap();
    assert_eq!(p.write_i32(1), Err(BAD_VALUE));
    assert_eq!(p.set_data_position(i32::MAX as usize + 1), Err(BAD_VALUE));
}

#[test]
fn set_data_size_truncates_and_clamps_the_position() {
    let mut p = P::new();
    for value in 0..4 {
        p.write_i32(value).unwrap();
    }
    p.set_data_size(8).unwrap();
    assert_eq!((p.data_size(), p.data_position()), (8, 8));
    p.set_data_size(64).unwrap();
    assert_eq!((p.data_size(), p.data_capacity()), (64, 128));
    assert_eq!(p.set_data_size(i32::MAX as usize + 1), Err(BAD_VALUE));
    p.set_data_capacity(16).unwrap();
    assert_eq!(p.data_capacity(), 128, "capacity never shrinks");
    p.set_data_capacity(1000).unwrap();
    assert_eq!(p.data_capacity(), 1000);
}

#[test]
fn a_huge_capacity_fails_instead_of_aborting() {
    let mut p = P::new();
    assert_eq!(p.set_data_capacity(i32::MAX as usize + 1), Err(BAD_VALUE));
    assert_eq!(p.set_data_size(i32::MAX as usize + 1), Err(BAD_VALUE));
    assert_eq!(p.data_capacity(), 0);
}

#[test]
fn string16_is_a_char_count_utf16_a_terminator_and_padding() {
    let mut p = P::new();
    p.write_string16(Some(&[u16::from(b'a'), u16::from(b'b')]))
        .unwrap();
    p.write_string16(None).unwrap();
    p.write_string16(Some(&[])).unwrap();
    assert_eq!(
        bytes(&p),
        [
            2, 0, 0, 0, b'a', 0, b'b', 0, 0, 0, 0, 0, // "ab", NUL, 2 bytes of padding
            0xff, 0xff, 0xff, 0xff, // null
            0, 0, 0, 0, 0, 0, 0, 0, // "", NUL, padding
        ]
    );
    p.set_data_position(0).unwrap();
    assert_eq!(
        p.read_string16(),
        Some(vec![u16::from(b'a'), u16::from(b'b')])
    );
    assert_eq!(p.read_string16(), None);
    assert_eq!(p.read_string16(), Some(vec![]));
}

#[test]
fn string8_is_a_byte_count_bytes_a_terminator_and_padding() {
    let mut p = P::new();
    p.write_string8(Some(b"abc")).unwrap();
    p.write_string8(Some(b"abcd")).unwrap();
    assert_eq!(
        bytes(&p),
        [
            3, 0, 0, 0, b'a', b'b', b'c', 0, 4, 0, 0, 0, b'a', b'b', b'c', b'd', 0, 0, 0, 0
        ]
    );
    p.set_data_position(0).unwrap();
    assert_eq!(p.read_string8(), Some(&b"abc"[..]));
    assert_eq!(p.read_string8(), Some(&b"abcd"[..]));
}

#[test]
fn hostile_string_lengths_read_nothing() {
    for length in [i32::MAX, i32::MAX - 1, 1 << 30, 3, -2] {
        let mut p = P::new();
        p.write_i32(length).unwrap();
        p.write_i32(0x4141_4141).unwrap();
        p.set_data_position(0).unwrap();
        assert_eq!(p.read_string16(), None, "length {length}");
        p.set_data_position(0).unwrap();
        assert_eq!(p.read_string8(), None, "length {length}");
    }
    // A string whose terminator is not NUL is refused, as AOSP's readString16Inplace does.
    let mut p = P::new();
    p.write_i32(1).unwrap();
    p.write_i32(0x0041_0041).unwrap();
    p.set_data_position(0).unwrap();
    assert_eq!(p.read_string16(), None);
}

#[test]
fn byte_arrays_and_blobs_are_length_prefixed() {
    let mut p = P::new();
    p.write_byte_array(Some(&[1, 2, 3])).unwrap();
    p.write_byte_array(None).unwrap();
    p.write_blob(Some(&[9, 8, 7, 6, 5])).unwrap();
    assert_eq!(
        bytes(&p),
        [
            3, 0, 0, 0, 1, 2, 3, 0, // length, bytes, padding
            0xff, 0xff, 0xff, 0xff, // null
            5, 0, 0, 0, 0, 0, 0, 0, 9, 8, 7, 6, 5, 0, 0, 0, // length, BLOB_INPLACE, bytes
        ]
    );
    p.set_data_position(0).unwrap();
    assert_eq!(p.create_byte_array(), Some(&[1, 2, 3][..]));
    assert_eq!(p.create_byte_array(), None);
    assert_eq!(p.read_blob(), Ok(Some(&[9, 8, 7, 6, 5][..])));

    p.set_data_position(0).unwrap();
    assert_eq!(
        p.read_byte_array(2),
        None,
        "the destination length must match"
    );
    p.set_data_position(0).unwrap();
    assert_eq!(p.read_byte_array(3), Some(&[1, 2, 3][..]));
}

#[test]
fn hostile_array_lengths_allocate_nothing() {
    let mut p = P::new();
    p.write_i32(i32::MAX).unwrap();
    p.set_data_position(0).unwrap();
    assert_eq!(p.create_byte_array(), None);
    p.write_i32(0).unwrap();
    p.set_data_position(0).unwrap();
    assert_eq!(p.read_blob(), Err(BAD_VALUE));
    // An ashmem blob cannot come from this process's writers, only from foreign bytes.
    let mut p = P::new();
    p.write_i32(4).unwrap();
    p.write_i32(1).unwrap();
    p.set_data_position(0).unwrap();
    assert_eq!(p.read_blob(), Err(BAD_VALUE));
}

#[test]
fn the_interface_token_carries_the_android_16_header() {
    let mut p = P::new();
    p.write_interface_token(&[u16::from(b'x')]).unwrap();
    assert_eq!(
        bytes(&p),
        [
            0, 0, 0, 0x80, // strict mode policy with PENALTY_GATHER
            0xff, 0xff, 0xff, 0xff, // work source: unset
            b'T', b'S', b'Y', b'S', // kHeader 'SYST'
            1, 0, 0, 0, b'x', 0, 0, 0, // the descriptor as String16
        ]
    );
    assert_eq!(p.read_calling_work_source_uid(), -1);
    assert!(p.replace_calling_work_source_uid(1234));
    assert_eq!(p.read_calling_work_source_uid(), 1234);
    assert_eq!(
        p.data_position(),
        20,
        "work source access keeps the position"
    );

    p.set_data_position(0).unwrap();
    assert!(p.enforce_interface(&[u16::from(b'x')]));
    p.set_data_position(0).unwrap();
    assert!(!p.enforce_interface(&[u16::from(b'y')]));

    let mut plain = P::new();
    assert!(!plain.replace_calling_work_source_uid(1));
    assert_eq!(plain.read_calling_work_source_uid(), -1);
    plain.write_i32(0).unwrap();
    plain.write_i32(0).unwrap();
    plain.write_i32(0).unwrap();
    plain.set_data_position(0).unwrap();
    assert!(!plain.enforce_interface(&[]), "a wrong header is refused");
}

#[test]
fn binders_are_flat_binder_objects_with_a_stability_word() {
    let mut p = P::new();
    p.write_binder(Some("service")).unwrap();
    p.write_binder(None).unwrap();
    let data = bytes(&p);
    assert_eq!(data.len(), 2 * (OBJECT_SIZE + 4));
    assert_eq!(&data[..8], [0x85, b'*', b'b', b's', 0x13, 0x01, 0, 0]);
    assert_eq!(&data[24..28], [12, 0, 0, 0], "stability SYSTEM");
    assert_eq!(&data[28..36], [0x85, b'*', b'b', b's', 0x13, 0, 0, 0]);
    assert_eq!(&data[36..56], [0; 20]);
    assert!(p.has_binders());
    assert!(!p.has_file_descriptors());

    p.set_data_position(0).unwrap();
    assert_eq!(p.read_binder(), Some("service"));
    assert_eq!(p.read_binder(), None);
    assert_eq!(p.data_avail(), 0);
    assert_eq!(
        p.marshall(),
        Err(INVALID_OPERATION),
        "binders cannot leave the process"
    );
}

#[test]
fn objects_are_found_only_where_they_were_written() {
    let mut p = P::new();
    p.write_i32(0).unwrap();
    p.write_binder(Some("service")).unwrap();
    let forged = p.marshall_unchecked();
    // The same bytes, from outside: no object table, so nothing is found.
    let mut q = P::new();
    q.unmarshall(&forged).unwrap();
    q.set_data_position(4).unwrap();
    assert_eq!(q.read_binder(), None);
    q.set_data_position(4).unwrap();
    assert!(q.read_file_descriptor().is_none());
    // A misaligned read in the original finds nothing either.
    p.set_data_position(0).unwrap();
    assert_eq!(p.read_binder(), None);
}

#[test]
fn file_descriptors_are_shared_in_process_and_not_marshalled() {
    let fd = std::fs::File::open("/dev/null").unwrap();
    let mut p = P::new();
    p.write_file_descriptor(fd.into()).unwrap();
    let data = bytes(&p);
    assert_eq!(&data[..8], [0x85, b'*', b'd', b'f', 0, 0, 0, 0]);
    assert_eq!(
        &data[16..24],
        [1, 0, 0, 0, 0, 0, 0, 0],
        "owned by the parcel"
    );
    assert!(p.has_file_descriptors());
    assert_eq!(p.has_file_descriptors_in_range(0, 24), Ok(true));
    assert_eq!(p.has_file_descriptors_in_range(4, 20), Ok(false));
    assert_eq!(p.has_file_descriptors_in_range(0, 25), Err(BAD_VALUE));
    p.set_data_position(0).unwrap();
    assert!(p.read_file_descriptor().is_some());
    assert_eq!(p.marshall(), Err(INVALID_OPERATION));

    let previous = p.push_allow_fds(false);
    assert!(previous);
    let fd = std::fs::File::open("/dev/null").unwrap();
    assert_eq!(p.write_file_descriptor(fd.into()), Err(FDS_NOT_ALLOWED));
    p.restore_allow_fds(previous);
}

#[test]
fn marshall_and_unmarshall_round_trip() {
    let mut p = P::new();
    p.write_i32(5).unwrap();
    p.write_string16(Some(&[0x263a])).unwrap();
    let flat = p.marshall().unwrap();
    let mut q = P::new();
    q.unmarshall(&flat).unwrap();
    assert_eq!((q.data_size(), q.data_position()), (flat.len(), flat.len()));
    q.set_data_position(0).unwrap();
    assert_eq!(q.read_i32(), Ok(5));
    assert_eq!(q.read_string16(), Some(vec![0x263a]));
    assert_eq!(p.compare_data(&q), 0);
    q.write_i32(1).unwrap();
    assert_eq!(p.compare_data(&q), -1);
    // writeInplace pads, so five bytes come back as eight.
    q.unmarshall(&[1, 2, 3, 4, 5]).unwrap();
    assert_eq!(q.data_size(), 8);
}

#[test]
fn append_from_copies_bytes_and_the_objects_inside_the_range() {
    let mut source = P::new();
    source.write_i32(1).unwrap();
    source.write_binder(Some("a")).unwrap();
    source.write_i32(2).unwrap();
    let mut target = P::new();
    target.write_i32(9).unwrap();
    target.append_from(&source, 4, 28).unwrap();
    assert_eq!(target.data_size(), 32);
    target.set_data_position(4).unwrap();
    assert_eq!(target.read_binder(), Some("a"));
    // An object cut by the range is not copied.
    let mut cut = P::new();
    cut.append_from(&source, 8, 24).unwrap();
    assert!(!cut.has_binders());
    assert_eq!(cut.append_from(&source, 4, 100), Err(BAD_VALUE));
    assert_eq!(cut.append_from(&source, usize::MAX, 4), Err(BAD_VALUE));
    assert_eq!(
        cut.append_from(&source, 0, i32::MAX as usize + 1),
        Err(BAD_VALUE)
    );

    let mut range = P::new();
    range.write_i32(1).unwrap();
    range.write_i32(2).unwrap();
    assert_eq!(source.compare_data_in_range(0, &range, 0, 4), Ok(true));
    assert_eq!(source.compare_data_in_range(32, &range, 4, 4), Ok(true));
    assert_eq!(
        source.compare_data_in_range(32, &range, 4, 8),
        Err(BAD_VALUE)
    );
}

#[test]
fn shrinking_drops_objects_past_the_end_and_free_data_resets() {
    let mut p = P::new();
    p.write_binder(Some("a")).unwrap();
    p.set_data_size(OBJECT_SIZE - 1).unwrap();
    assert!(!p.has_binders());
    p.write_i32(3).unwrap();
    p.free_data();
    assert_eq!(
        (p.data_size(), p.data_position(), p.data_capacity()),
        (0, 0, 0)
    );
    assert!(p.push_allow_fds(true));
}

#[test]
fn modified_utf8_decodes_to_utf16_without_trusting_the_bytes() {
    assert_eq!(utf16_from_modified_utf8(b"ab"), vec![0x61, 0x62]);
    // U+0000 as C0 80, U+263A, and a surrogate pair encoded one half at a time.
    assert_eq!(
        utf16_from_modified_utf8(&[
            0xc0, 0x80, 0xe2, 0x98, 0xba, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80
        ]),
        vec![0, 0x263a, 0xd83d, 0xde00]
    );
    // Four-byte UTF-8 from a foreign writer, truncated and stray bytes.
    assert_eq!(
        utf16_from_modified_utf8(&[0xf0, 0x9f, 0x98, 0x80]),
        vec![0xd83d, 0xde00]
    );
    assert_eq!(
        utf16_from_modified_utf8(&[0xe2, 0x98, 0x80, 0xff, 0xe2]),
        vec![0x2600, 0xfffd, 0xfffd]
    );
}

#[test]
fn random_bytes_and_positions_never_panic() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..20_000 {
        let length = (next() % 48) as usize;
        let data: Vec<u8> = (0..length).map(|_| next() as u8).collect();
        let mut p = P::new();
        p.unmarshall(&data).unwrap();
        if next() % 2 == 0 {
            p.write_binder(Some("b")).unwrap();
        }
        for _ in 0..8 {
            let _ = p.set_data_position((next() % 64) as usize);
            match next() % 10 {
                0 => drop(p.read_i32()),
                1 => drop(p.read_i64()),
                2 => drop(p.read_string16()),
                3 => drop(p.read_string8().map(<[u8]>::to_vec)),
                4 => drop(p.create_byte_array().map(<[u8]>::to_vec)),
                5 => drop(p.read_blob().map(|b| b.map(<[u8]>::to_vec))),
                6 => drop(p.read_binder()),
                7 => drop(p.read_file_descriptor()),
                8 => drop(p.enforce_interface(&[0x61])),
                _ => drop(p.has_binders_in_range((next() % 64) as usize, (next() % 64) as usize)),
            }
        }
        let _ = utf16_from_modified_utf8(&data);
    }
}
