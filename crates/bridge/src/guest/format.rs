//! The argument classes a printf format consumes, which is how a variadic call is moved from the
//! guest's registers to the host's.

use super::abi::Class;

/// Formats are read at most this far, which bounds the work a hostile string can cause.
pub const MAX_FORMAT_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum FormatError {
    /// `%Lf` and its kind: the guest's `long double` is binary128, the host's x87.
    LongDouble,
    /// Positional arguments (`%1$d`), which would need the whole format to place.
    Positional,
    TooLong,
}

/// The classes of the arguments `format` reads, in order.
pub fn classes(format: &[u8]) -> Result<Vec<Class>, FormatError> {
    if format.len() > MAX_FORMAT_BYTES {
        return Err(FormatError::TooLong);
    }
    let mut classes = Vec::new();
    let mut bytes = format.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            continue;
        }
        let mut long_double = false;
        let mut digits = false;
        loop {
            let Some(next) = bytes.next() else {
                return Ok(classes);
            };
            match next {
                b'%' if !digits => break,
                b'$' if digits => return Err(FormatError::Positional),
                b'*' => classes.push(Class::Integer),
                b'0'..=b'9' => digits = true,
                b'-' | b'+' | b' ' | b'#' | b'.' | b'\'' | b'h' | b'l' | b'j' | b'z' | b't'
                | b'q' => {}
                b'L' => long_double = true,
                b'f' | b'F' | b'e' | b'E' | b'g' | b'G' | b'a' | b'A' => {
                    if long_double {
                        return Err(FormatError::LongDouble);
                    }
                    classes.push(Class::Vector);
                    break;
                }
                b'd' | b'i' | b'u' | b'o' | b'x' | b'X' | b'c' | b's' | b'p' | b'n' | b'C'
                | b'S' => {
                    classes.push(Class::Integer);
                    break;
                }
                // `%m` prints errno and reads nothing; anything unknown also ends the directive.
                _ => break,
            }
        }
    }
    Ok(classes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Class::{Integer as I, Vector as V};

    #[test]
    fn conversions_map_to_their_classes() {
        assert_eq!(
            classes(b"int %d, string %s, double %.2f, long %ld, %%, %m, %p").unwrap(),
            vec![I, I, V, I, I]
        );
        assert_eq!(
            classes(b"%*.*s %-08.3e %hhu %zx %c").unwrap(),
            vec![I, I, I, V, I, I, I]
        );
        assert_eq!(classes(b"trailing %").unwrap(), vec![]);
    }

    #[test]
    fn formats_the_host_cannot_carry_are_refused() {
        assert_eq!(classes(b"%Lf"), Err(FormatError::LongDouble));
        assert_eq!(classes(b"%2$d %1$d"), Err(FormatError::Positional));
        assert_eq!(
            classes(&vec![b'a'; MAX_FORMAT_BYTES + 1]),
            Err(FormatError::TooLong)
        );
    }
}
