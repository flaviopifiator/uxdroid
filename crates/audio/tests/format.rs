use uxdroid_audio::Format;

fn decoded(format: Format, bytes: &[u8]) -> Vec<f32> {
    let mut out = vec![9.0; bytes.len() / format.bytes()];
    assert_eq!(format.decode(bytes, &mut out), out.len());
    out
}

fn encoded(format: Format, samples: &[f32]) -> Vec<u8> {
    let mut out = vec![0xAA; samples.len() * format.bytes()];
    assert_eq!(format.encode(samples, &mut out), samples.len());
    out
}

#[test]
fn integer_formats_decode_to_unit_floats() {
    let i16s: Vec<u8> = [0i16, 16384, -32768, 32767]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    assert_eq!(
        decoded(Format::I16, &i16s),
        [0.0, 0.5, -1.0, 32767.0 / 32768.0]
    );
    assert_eq!(
        decoded(Format::U8, &[128, 192, 0, 255]),
        [0.0, 0.5, -1.0, 127.0 / 128.0]
    );
    // 24-bit packed, little-endian: 0x400000 is half scale, 0x800000 is the minimum.
    assert_eq!(
        decoded(Format::I24, &[0, 0, 0x40, 0, 0, 0x80, 0xff, 0xff, 0xff]),
        [0.5, -1.0, -1.0 / 8_388_608.0]
    );
    let i32s: Vec<u8> = [i32::MIN, 1 << 30]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    assert_eq!(decoded(Format::I32, &i32s), [-1.0, 0.5]);
    let f32s: Vec<u8> = [0.25f32, -0.75]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    assert_eq!(decoded(Format::F32, &f32s), [0.25, -0.75]);
}

#[test]
fn floats_encode_with_rounding_and_clipping() {
    let i16s = encoded(Format::I16, &[0.5, -1.0, 1.0, 2.0, 0.000_02]);
    let back: Vec<i16> = i16s
        .chunks(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    assert_eq!(back, [16384, -32768, 32767, 32767, 1]);
    assert_eq!(
        encoded(Format::U8, &[0.0, -1.0, 1.0, 0.5]),
        [128, 0, 255, 192]
    );
    assert_eq!(encoded(Format::I24, &[0.5, -1.0]), [0, 0, 0x40, 0, 0, 0x80]);
    let i32s = encoded(Format::I32, &[-1.0, 1.0]);
    assert_eq!(&i32s[..4], i32::MIN.to_le_bytes());
    assert_eq!(&i32s[4..], i32::MAX.to_le_bytes());
    assert_eq!(encoded(Format::F32, &[0.25]), 0.25f32.to_le_bytes());
}

#[test]
fn partial_input_stops_at_the_shorter_side() {
    let mut out = [7.0f32; 4];
    // Three bytes hold one whole 16-bit sample.
    assert_eq!(Format::I16.decode(&[0, 0x40, 0], &mut out), 1);
    assert_eq!(out, [0.5, 7.0, 7.0, 7.0]);
    let mut small = [0u8; 2];
    assert_eq!(Format::I16.encode(&[0.5, 0.5], &mut small), 1);
}
