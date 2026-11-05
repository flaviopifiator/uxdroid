//! The records an input channel carries, one per datagram of a `SOCK_SEQPACKET` pair.
//!
//! Android's channel carries `InputMessage`s between two processes built from the same tree. Both
//! ends here are this library in one process, so the record is ours: little-endian fields, a kind,
//! the sequence number the receiver finishes, and the event. A record that is short, long, of an
//! unknown kind, or with more pointers than Android allows decodes to nothing.

use super::{Event, Key, Motion, Pointer};

/// `MotionEvent` holds at most this many pointers, `MAX_POINTERS` in AOSP's `Input.h`.
pub const MAX_POINTERS: usize = 16;

const KEY: u32 = 1;
const MOTION: u32 = 2;
const FOCUS: u32 = 3;

/// The largest record: a motion event with every pointer.
pub const MAX_RECORD: usize = 8 + 16 + 8 * 4 + 4 + MAX_POINTERS * 20;

/// Encodes an event with its sequence number.
pub fn encode(seq: u32, event: &Event) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    let kind = match event {
        Event::Key(_) => KEY,
        Event::Motion(_) => MOTION,
        Event::Focus(_) => FOCUS,
    };
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    let int = |out: &mut Vec<u8>, value: i32| out.extend_from_slice(&value.to_le_bytes());
    let float = |out: &mut Vec<u8>, value: f32| out.extend_from_slice(&value.to_le_bytes());
    match event {
        Event::Key(key) => {
            out.extend_from_slice(&key.down_time.to_le_bytes());
            out.extend_from_slice(&key.event_time.to_le_bytes());
            for value in [
                key.action,
                key.key_code,
                key.scan_code,
                key.meta_state,
                key.repeat,
                key.flags,
                key.source,
                key.device,
            ] {
                int(&mut out, value);
            }
        }
        Event::Motion(motion) => {
            out.extend_from_slice(&motion.down_time.to_le_bytes());
            out.extend_from_slice(&motion.event_time.to_le_bytes());
            for value in [
                motion.action,
                motion.action_button,
                motion.button_state,
                motion.meta_state,
                motion.source,
                motion.device,
            ] {
                int(&mut out, value);
            }
            float(&mut out, motion.vscroll);
            float(&mut out, motion.hscroll);
            let pointers = &motion.pointers[..motion.pointers.len().min(MAX_POINTERS)];
            int(&mut out, pointers.len() as i32);
            for pointer in pointers {
                int(&mut out, pointer.id);
                int(&mut out, pointer.tool);
                float(&mut out, pointer.x);
                float(&mut out, pointer.y);
                float(&mut out, pointer.pressure);
            }
        }
        Event::Focus(focused) => int(&mut out, i32::from(*focused)),
    }
    out
}

/// Reads fields in order, failing once the record runs out.
struct Reader<'a> {
    rest: &'a [u8],
}

impl Reader<'_> {
    fn bytes<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.rest.split_first_chunk::<N>()?;
        self.rest = rest;
        Some(*head)
    }

    fn u32(&mut self) -> Option<u32> {
        self.bytes().map(u32::from_le_bytes)
    }

    fn i32(&mut self) -> Option<i32> {
        self.bytes().map(i32::from_le_bytes)
    }

    fn i64(&mut self) -> Option<i64> {
        self.bytes().map(i64::from_le_bytes)
    }

    fn f32(&mut self) -> Option<f32> {
        self.bytes().map(f32::from_le_bytes)
    }
}

/// Decodes a record into its sequence number and event.
pub fn decode(record: &[u8]) -> Option<(u32, Event)> {
    let mut reader = Reader { rest: record };
    let kind = reader.u32()?;
    let seq = reader.u32()?;
    let event = match kind {
        KEY => Event::Key(Key {
            down_time: reader.i64()?,
            event_time: reader.i64()?,
            action: reader.i32()?,
            key_code: reader.i32()?,
            scan_code: reader.i32()?,
            meta_state: reader.i32()?,
            repeat: reader.i32()?,
            flags: reader.i32()?,
            source: reader.i32()?,
            device: reader.i32()?,
        }),
        MOTION => {
            let (down_time, event_time) = (reader.i64()?, reader.i64()?);
            let (action, action_button, button_state) =
                (reader.i32()?, reader.i32()?, reader.i32()?);
            let (meta_state, source, device) = (reader.i32()?, reader.i32()?, reader.i32()?);
            let (vscroll, hscroll) = (reader.f32()?, reader.f32()?);
            let count = usize::try_from(reader.i32()?).ok()?;
            if !(1..=MAX_POINTERS).contains(&count) {
                return None;
            }
            let pointers = (0..count)
                .map(|_| {
                    Some(Pointer {
                        id: reader.i32()?,
                        tool: reader.i32()?,
                        x: reader.f32()?,
                        y: reader.f32()?,
                        pressure: reader.f32()?,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Event::Motion(Motion {
                down_time,
                event_time,
                action,
                action_button,
                button_state,
                meta_state,
                source,
                device,
                vscroll,
                hscroll,
                pointers,
            })
        }
        FOCUS => match reader.i32()? {
            0 => Event::Focus(false),
            1 => Event::Focus(true),
            _ => return None,
        },
        _ => return None,
    };
    reader.rest.is_empty().then_some((seq, event))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(pointers: usize) -> Event {
        Event::Motion(Motion {
            down_time: 1_000_000_000,
            event_time: 1_016_000_000,
            action: 5 | 1 << 8,
            action_button: 1,
            button_state: 1,
            meta_state: 0x41,
            source: super::super::SOURCE_TOUCHSCREEN,
            device: 3,
            vscroll: -1.5,
            hscroll: 0.25,
            pointers: (0..pointers)
                .map(|i| Pointer {
                    id: i as i32,
                    tool: 1,
                    x: 10.5 + i as f32,
                    y: 600.25,
                    pressure: 1.0,
                })
                .collect(),
        })
    }

    #[test]
    fn every_event_survives_a_round_trip() {
        let key = Event::Key(Key {
            down_time: 5,
            event_time: i64::MAX,
            action: 1,
            key_code: 29,
            scan_code: 30,
            meta_state: 0x100041,
            repeat: 2,
            flags: 8,
            source: super::super::SOURCE_KEYBOARD,
            device: 1,
        });
        for (seq, event) in [
            (1, key),
            (u32::MAX, motion(1)),
            (7, motion(MAX_POINTERS)),
            (8, Event::Focus(true)),
            (9, Event::Focus(false)),
        ] {
            let record = encode(seq, &event);
            assert!(record.len() <= MAX_RECORD);
            assert_eq!(decode(&record), Some((seq, event)));
        }
    }

    #[test]
    fn malformed_records_decode_to_nothing() {
        let good = encode(3, &motion(2));
        for length in 0..good.len() {
            assert_eq!(decode(&good[..length]), None, "truncated to {length}");
        }
        let mut long = good.clone();
        long.push(0);
        assert_eq!(decode(&long), None);
        let mut unknown = good.clone();
        unknown[0] = 99;
        assert_eq!(decode(&unknown), None);
        // A pointer count past Android's limit, or negative, is refused before any allocation.
        let count_at = good.len() - 2 * 20 - 4;
        for count in [-1i32, 0, MAX_POINTERS as i32 + 1, i32::MAX] {
            let mut forged = good.clone();
            forged[count_at..count_at + 4].copy_from_slice(&count.to_le_bytes());
            assert_eq!(decode(&forged), None, "pointer count {count}");
        }
        let focus = encode(1, &Event::Focus(true));
        let mut bad_focus = focus.clone();
        bad_focus[8] = 2;
        assert_eq!(decode(&bad_focus), None);
    }
}
