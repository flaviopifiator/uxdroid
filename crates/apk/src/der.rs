//! A reader for the DER that X.509 certificates and PKCS #7 signature blocks use.
//!
//! Only definite lengths and one-byte tags are accepted, which is all DER allows; every length is
//! checked against what remains, so a crafted length cannot reach past the input.

pub(crate) const SEQUENCE: u8 = 0x30;
pub(crate) const SET: u8 = 0x31;
pub(crate) const INTEGER: u8 = 0x02;
pub(crate) const OCTET_STRING: u8 = 0x04;
pub(crate) const OID: u8 = 0x06;

/// One element: its tag, its content, and its whole encoding with tag and length.
#[derive(Clone, Copy)]
pub(crate) struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub whole: &'a [u8],
}

/// Reads the elements of a content one after another.
pub(crate) struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn next(&mut self) -> Result<Tlv<'a>, &'static str> {
        let input = self.0;
        let (&tag, rest) = input.split_first().ok_or("truncated DER")?;
        if tag & 0x1f == 0x1f {
            return Err("unsupported DER tag");
        }
        let (&first, rest) = rest.split_first().ok_or("truncated DER")?;
        let (length, rest) = match first {
            0..=0x7f => (usize::from(first), rest),
            0x81..=0x84 => {
                let count = usize::from(first & 0x7f);
                let bytes = rest.get(..count).ok_or("truncated DER")?;
                let length = bytes
                    .iter()
                    .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
                (length, &rest[count..])
            }
            _ => return Err("indefinite or oversized DER length"),
        };
        let content = rest.get(..length).ok_or("truncated DER")?;
        let consumed = input.len() - rest.len() + length;
        self.0 = &rest[length..];
        Ok(Tlv {
            tag,
            content,
            whole: &input[..consumed],
        })
    }

    pub fn expect(&mut self, tag: u8) -> Result<Tlv<'a>, &'static str> {
        let element = self.next()?;
        if element.tag == tag {
            Ok(element)
        } else {
            Err("unexpected DER element")
        }
    }

    /// The next element if it has `tag`; otherwise nothing is consumed.
    pub fn optional(&mut self, tag: u8) -> Result<Option<Tlv<'a>>, &'static str> {
        if self.0.first() == Some(&tag) {
            self.next().map(Some)
        } else {
            Ok(None)
        }
    }
}

/// Encodes an object identifier written with dots, for comparison with the content of an `OID`.
pub(crate) fn oid(text: &str) -> Vec<u8> {
    let arcs: Vec<u64> = text
        .split('.')
        .map(|arc| arc.parse().unwrap_or(0))
        .collect();
    let mut out = Vec::new();
    let mut push = |mut arc: u64| {
        let mut bytes = vec![(arc & 0x7f) as u8];
        arc >>= 7;
        while arc > 0 {
            bytes.push((arc & 0x7f) as u8 | 0x80);
            arc >>= 7;
        }
        out.extend(bytes.iter().rev());
    };
    if let [first, second, rest @ ..] = arcs.as_slice() {
        push(first * 40 + second);
        rest.iter().for_each(|arc| push(*arc));
    }
    out
}

/// The parts of an X.509 certificate a signature check needs.
pub(crate) struct Certificate<'a> {
    pub serial: &'a [u8],
    pub issuer: &'a [u8],
    /// The whole `SubjectPublicKeyInfo`.
    pub public_key: &'a [u8],
}

pub(crate) fn certificate(der: &[u8]) -> Result<Certificate<'_>, &'static str> {
    let mut outer = Reader::new(der);
    let certificate = outer.expect(SEQUENCE)?;
    if !outer.is_empty() {
        return Err("data after the certificate");
    }
    let mut fields = Reader::new(certificate.content);
    let tbs = fields.expect(SEQUENCE)?;
    let mut tbs = Reader::new(tbs.content);
    tbs.optional(0xa0)?;
    let serial = tbs.expect(INTEGER)?.content;
    tbs.expect(SEQUENCE)?;
    let issuer = tbs.expect(SEQUENCE)?.whole;
    tbs.expect(SEQUENCE)?;
    tbs.expect(SEQUENCE)?;
    let public_key = tbs.expect(SEQUENCE)?.whole;
    Ok(Certificate {
        serial,
        issuer,
        public_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_identifiers_encode_as_der_does() {
        assert_eq!(
            oid("1.2.840.113549.1.7.2"),
            [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02]
        );
    }

    #[test]
    fn lengths_past_the_input_are_refused() {
        for bytes in [
            &[0x30, 0x05, 0x00][..],
            &[0x30, 0x84, 0xff, 0xff, 0xff, 0xff],
            &[0x30, 0x80, 0x00, 0x00],
            &[0x30],
            &[0x1f, 0x01, 0x00],
        ] {
            assert!(Reader::new(bytes).next().is_err(), "{bytes:?}");
        }
        let mut reader = Reader::new(&[0x02, 0x01, 0x07, 0x04, 0x00]);
        assert_eq!(reader.expect(INTEGER).unwrap().content, [7]);
        assert_eq!(reader.expect(OCTET_STRING).unwrap().whole, [4, 0]);
        assert!(reader.is_empty());
    }
}
