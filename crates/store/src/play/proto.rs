//! The protocol buffer wire format, read and written by hand for the few fields the Play source uses.
//!
//! Everything read comes from a server, so nothing here trusts a length: every field is checked
//! against the bytes left, a varint longer than ten bytes or a field number of 0 is refused, and so
//! are groups, which Google's responses do not use. Messages are only parsed when a caller asks for
//! a field inside them, so the work is linear in what is read and nesting is bounded by the code that
//! walks it, never by the input.

use crate::Error;

/// One value on the wire. Length-delimited values stay bytes until read as a string or a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

/// A message: its encoded bytes, read field by field.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Message<'a> {
    bytes: &'a [u8],
}

fn malformed(why: &str) -> Error {
    Error::new(format!(
        "Google Play answered with a malformed message ({why}), so it is refused"
    ))
}

fn varint(bytes: &[u8], at: &mut usize) -> Result<u64, Error> {
    let mut value = 0u64;
    for shift in 0..10 {
        let byte = *bytes
            .get(*at)
            .ok_or_else(|| malformed("a number is cut short"))?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(malformed("a number is longer than ten bytes"))
}

fn take<'a>(bytes: &'a [u8], at: &mut usize, length: u64) -> Result<&'a [u8], Error> {
    let left = bytes.len() - *at;
    let length = usize::try_from(length)
        .ok()
        .filter(|length| *length <= left)
        .ok_or_else(|| malformed("a field is longer than what is left of the message"))?;
    let taken = &bytes[*at..*at + length];
    *at += length;
    Ok(taken)
}

impl<'a> Message<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    /// Every field in order, or the first error, after which nothing more is read.
    pub(crate) fn fields(&self) -> Fields<'a> {
        Fields {
            bytes: self.bytes,
            at: 0,
            failed: false,
        }
    }

    /// Checks that the whole message is well formed, without reading inside nested messages.
    pub(crate) fn check(&self) -> Result<(), Error> {
        self.fields().try_for_each(|field| field.map(|_| ()))
    }

    /// The values of field `number`, in order.
    pub(crate) fn all(&self, number: u32) -> Result<Vec<Value<'a>>, Error> {
        let mut found = Vec::new();
        for field in self.fields() {
            let (seen, value) = field?;
            if seen == number {
                found.push(value);
            }
        }
        Ok(found)
    }

    /// The last value of field `number`, as protocol buffers read a field given more than once.
    pub(crate) fn get(&self, number: u32) -> Result<Option<Value<'a>>, Error> {
        Ok(self.all(number)?.pop())
    }

    pub(crate) fn uint(&self, number: u32) -> Result<Option<u64>, Error> {
        match self.get(number)? {
            None => Ok(None),
            Some(Value::Varint(value) | Value::Fixed64(value)) => Ok(Some(value)),
            Some(Value::Fixed32(value)) => Ok(Some(u64::from(value))),
            Some(Value::Bytes(_)) => Err(malformed("a number is given as bytes")),
        }
    }

    pub(crate) fn bytes(&self, number: u32) -> Result<Option<&'a [u8]>, Error> {
        match self.get(number)? {
            None => Ok(None),
            Some(Value::Bytes(bytes)) => Ok(Some(bytes)),
            Some(_) => Err(malformed("bytes are given as a number")),
        }
    }

    pub(crate) fn string(&self, number: u32) -> Result<Option<&'a str>, Error> {
        self.bytes(number)?
            .map(|bytes| std::str::from_utf8(bytes).map_err(|_| malformed("a text is not UTF-8")))
            .transpose()
    }

    pub(crate) fn message(&self, number: u32) -> Result<Option<Message<'a>>, Error> {
        Ok(self.bytes(number)?.map(Message::new))
    }

    /// Every value of field `number` as a message.
    pub(crate) fn messages(&self, number: u32) -> Result<Vec<Message<'a>>, Error> {
        self.all(number)?
            .into_iter()
            .map(|value| match value {
                Value::Bytes(bytes) => Ok(Message::new(bytes)),
                _ => Err(malformed("a message is given as a number")),
            })
            .collect()
    }

    /// The message at `path`, one field number for each level, when every level is there.
    pub(crate) fn at(&self, path: &[u32]) -> Result<Option<Message<'a>>, Error> {
        let mut current = *self;
        for number in path {
            match current.message(*number)? {
                Some(next) => current = next,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }
}

pub(crate) struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
    failed: bool,
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u32, Value<'a>), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.at >= self.bytes.len() {
            return None;
        }
        let field = self.read();
        self.failed = field.is_err();
        Some(field)
    }
}

impl<'a> Fields<'a> {
    fn read(&mut self) -> Result<(u32, Value<'a>), Error> {
        let key = varint(self.bytes, &mut self.at)?;
        let number = u32::try_from(key >> 3)
            .ok()
            .filter(|number| *number != 0 && *number < 1 << 29)
            .ok_or_else(|| malformed("a field number is out of range"))?;
        let value = match key & 7 {
            0 => Value::Varint(varint(self.bytes, &mut self.at)?),
            1 => {
                let bytes = take(self.bytes, &mut self.at, 8)?;
                Value::Fixed64(u64::from_le_bytes(bytes.try_into().expect("eight bytes")))
            }
            2 => {
                let length = varint(self.bytes, &mut self.at)?;
                Value::Bytes(take(self.bytes, &mut self.at, length)?)
            }
            5 => {
                let bytes = take(self.bytes, &mut self.at, 4)?;
                Value::Fixed32(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
            }
            _ => return Err(malformed("a field has an unknown or group wire type")),
        };
        Ok((number, value))
    }
}

/// Builds a message, field by field.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn raw_varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.bytes.push((value as u8) | 0x80);
            value >>= 7;
        }
        self.bytes.push(value as u8);
    }

    fn key(&mut self, number: u32, wire: u64) {
        self.raw_varint((u64::from(number) << 3) | wire);
    }

    pub(crate) fn uint(&mut self, number: u32, value: u64) -> &mut Self {
        self.key(number, 0);
        self.raw_varint(value);
        self
    }

    pub(crate) fn boolean(&mut self, number: u32, value: bool) -> &mut Self {
        self.uint(number, u64::from(value))
    }

    pub(crate) fn bytes(&mut self, number: u32, value: &[u8]) -> &mut Self {
        self.key(number, 2);
        self.raw_varint(value.len() as u64);
        self.bytes.extend_from_slice(value);
        self
    }

    pub(crate) fn string(&mut self, number: u32, value: &str) -> &mut Self {
        self.bytes(number, value.as_bytes())
    }

    pub(crate) fn message(&mut self, number: u32, build: impl FnOnce(&mut Writer)) -> &mut Self {
        let mut inner = Writer::new();
        build(&mut inner);
        self.bytes(number, &inner.bytes)
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}

/// Base64 with the URL alphabet, padded or not, as Google gives hashes. Anything else is refused.
pub(crate) fn base64url(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    // Leftover bits must be padding zeros, and a single leftover character is not base64.
    (bits < 6 && buffer & ((1 << bits) - 1) == 0).then_some(out)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_messages_read_back() {
        let bytes = Writer::new()
            .uint(1, 300)
            .string(2, "net.example")
            .message(3, |inner| {
                inner.uint(1, 7).uint(2, u64::MAX);
            })
            .string(4, "a")
            .string(4, "b")
            .finish();
        let message = Message::new(&bytes);
        assert_eq!(message.uint(1).unwrap(), Some(300));
        assert_eq!(message.string(2).unwrap(), Some("net.example"));
        let inner = message.message(3).unwrap().unwrap();
        assert_eq!(inner.uint(1).unwrap(), Some(7));
        assert_eq!(inner.uint(2).unwrap(), Some(u64::MAX));
        assert_eq!(message.string(4).unwrap(), Some("b"));
        assert_eq!(message.all(4).unwrap().len(), 2);
        assert_eq!(message.uint(9).unwrap(), None);
    }

    #[test]
    fn damaged_messages_are_refused_without_panicking() {
        let good = Writer::new()
            .string(1, "net.example")
            .message(2, |inner| {
                inner.uint(1, 1);
            })
            .finish();
        for cut in 0..good.len() {
            let message = Message::new(&good[..cut]);
            let _ = message.check();
            let _ = message
                .at(&[2])
                .map(|inner| inner.map(|inner| inner.check()));
        }
        let cases: [&[u8]; 7] = [
            // A length of 2^63.
            &[
                0x0a, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
            ],
            // An eleven-byte varint.
            &[
                0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
            ],
            // Field number 0.
            &[0x00, 0x01],
            // A start group.
            &[0x0b],
            // Wire type 7.
            &[0x0f],
            // A fixed64 cut short.
            &[0x09, 1, 2, 3],
            // A length past the end.
            &[0x0a, 0x05, b'a'],
        ];
        for case in cases {
            assert!(Message::new(case).check().is_err(), "{case:02x?}");
        }
        let invalid_text = [0x0a, 0x02, 0xff, 0xfe];
        assert!(Message::new(&invalid_text).string(1).is_err());
        let number_as_bytes = [0x08, 0x01];
        assert!(Message::new(&number_as_bytes).message(1).is_err());
    }

    #[test]
    fn hashes_in_base64url_decode() {
        assert_eq!(
            hex(&base64url("V9EG0M-odjRCs2Re8nQcOLuCC9Vv1GEr9AojttmYvl4").unwrap()),
            "57d106d0cfa8763442b3645ef2741c38bb820bd56fd4612bf40a23b6d998be5e"
        );
        assert_eq!(base64url("YQ==").unwrap(), b"a");
        assert_eq!(base64url("YQ").unwrap(), b"a");
        assert!(base64url("Y").is_none());
        assert!(base64url("YR").is_none(), "nonzero padding bits");
        assert!(base64url("a b").is_none());
    }
}
