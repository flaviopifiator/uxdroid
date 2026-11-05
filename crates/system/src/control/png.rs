//! PNG images of the display for `screenshot`, and the base64 that carries them in a JSON answer.
//! Only what that needs: 8-bit RGB, no filter, one zlib stream.

use std::io::{Read, Write};

/// An RGB image, row after row, 3 bytes a pixel.
#[derive(Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let at = (y as usize * self.width as usize + x as usize) * 3;
        [self.rgb[at], self.rgb[at + 1], self.rgb[at + 2]]
    }
}

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc = crc32fast::Hasher::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc.finalize().to_be_bytes());
}

/// The bytes of an RGB image of this size, if PNG can hold it: both sides 1 to 2^31 - 1.
fn rgb_length(width: u32, height: u32) -> Option<usize> {
    let valid = |side: u32| (1..=i32::MAX as u32).contains(&side);
    if !valid(width) || !valid(height) {
        return None;
    }
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(3)
}

/// The image as a PNG file, or `None` when its pixels do not fill its size. Compression is fast
/// rather than small: a screenshot is taken while the agent waits.
pub fn encode(image: &Image) -> Option<Vec<u8>> {
    if rgb_length(image.width, image.height)? != image.rgb.len() {
        return None;
    }
    let row = image.width as usize * 3;
    let mut raw = Vec::with_capacity((row + 1) * image.height as usize);
    for line in image
        .rgb
        .chunks_exact(row.max(1))
        .take(image.height as usize)
    {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zlib.write_all(&raw)
        .expect("writing to memory does not fail");
    let compressed = zlib.finish().expect("writing to memory does not fail");
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&image.width.to_be_bytes());
    header.extend_from_slice(&image.height.to_be_bytes());
    // 8 bits a channel, truecolor, deflate, no filter method beyond the default, no interlace.
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = SIGNATURE.to_vec();
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &compressed);
    chunk(&mut out, b"IEND", &[]);
    Some(out)
}

/// Reads back a PNG this module wrote: 8-bit RGB with no row filters. `None` for anything else.
pub fn decode(data: &[u8]) -> Option<Image> {
    let mut rest = data.strip_prefix(&SIGNATURE)?;
    let (mut width, mut height, mut compressed) = (0, 0, Vec::new());
    while rest.len() >= 12 {
        let length = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let kind = &rest[4..8];
        let body = rest.get(8..8 + length)?;
        match kind {
            b"IHDR" if body.len() == 13 && body[8..13] == [8, 2, 0, 0, 0] => {
                width = u32::from_be_bytes(body[..4].try_into().ok()?);
                height = u32::from_be_bytes(body[4..8].try_into().ok()?);
            }
            b"IHDR" => return None,
            b"IDAT" => compressed.extend_from_slice(body),
            _ => {}
        }
        rest = rest.get(12 + length..)?;
    }
    let row = width as usize * 3;
    let expected = rgb_length(width, height)?.checked_add(height as usize)?;
    // Inflates no more than the size promises, so a small file cannot claim gigabytes.
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&compressed[..])
        .take(expected as u64 + 1)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() != expected {
        return None;
    }
    let mut rgb = Vec::with_capacity(row * height as usize);
    for line in raw.chunks_exact(row + 1) {
        if line[0] != 0 {
            return None;
        }
        rgb.extend_from_slice(&line[1..]);
    }
    Some(Image { width, height, rgb })
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, as MCP's image content and the protocol's answers carry it.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let bytes = [
            group[0],
            *group.get(1).unwrap_or(&0),
            *group.get(2).unwrap_or(&0),
        ];
        let bits = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
        for index in 0..4 {
            if index <= group.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Reads standard base64 with padding; `None` for anything else.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let value = |byte: u8| ALPHABET.iter().position(|&c| c == byte).map(|v| v as u32);
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let groups = bytes.len() / 4;
    for (index, group) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let padding = group.iter().rev().take_while(|&&b| b == b'=').count();
        if padding > 2 || (padding > 0 && index + 1 != groups) {
            return None;
        }
        let mut bits = 0u32;
        for &byte in &group[..4 - padding] {
            bits = bits << 6 | value(byte)?;
        }
        bits <<= 6 * padding as u32;
        let decoded = [(bits >> 16) as u8, (bits >> 8) as u8, bits as u8];
        out.extend_from_slice(&decoded[..3 - padding]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_and_refuses_what_is_not_base64() {
        for (data, text) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"\xff\x00\xfe", "/wD+"),
        ] {
            assert_eq!(base64_encode(data), text);
            assert_eq!(base64_decode(text).as_deref(), Some(data));
        }
        for bad in ["Zg=", "Z===", "Zg==Zg==", "Z!==", "\u{e9}AAA"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_encoded_image_is_a_png_that_reads_back() {
        let image = Image {
            width: 3,
            height: 2,
            rgb: (0..18).collect(),
        };
        let data = encode(&image).unwrap();
        assert_eq!(&data[..8], &SIGNATURE);
        assert_eq!(&data[12..16], b"IHDR");
        assert_eq!(decode(&data), Some(image));
        assert_eq!(decode(&data[..20]), None);
        assert_eq!(decode(b"not a png"), None);
    }

    #[test]
    fn pixels_that_do_not_fill_the_size_are_no_image() {
        let short = Image {
            width: 3,
            height: 2,
            rgb: vec![0; 17],
        };
        assert_eq!(encode(&short), None);
        let empty = Image {
            width: 0,
            height: 2,
            rgb: Vec::new(),
        };
        assert_eq!(encode(&empty), None);
    }

    #[test]
    fn a_png_claiming_a_huge_size_is_refused_without_overflowing() {
        let mut data = SIGNATURE.to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&u32::MAX.to_be_bytes());
        header.extend_from_slice(&u32::MAX.to_be_bytes());
        header.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut data, b"IHDR", &header);
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zlib.write_all(&[0; 64]).unwrap();
        chunk(&mut data, b"IDAT", &zlib.finish().unwrap());
        chunk(&mut data, b"IEND", &[]);
        assert_eq!(decode(&data), None);
        // A large size that fits is still bounded by what it promises.
        let mut data = SIGNATURE.to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&1u32.to_be_bytes());
        header.extend_from_slice(&1u32.to_be_bytes());
        header.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut data, b"IHDR", &header);
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zlib.write_all(&vec![0; 1 << 20]).unwrap();
        chunk(&mut data, b"IDAT", &zlib.finish().unwrap());
        assert_eq!(decode(&data), None);
    }
}
