//! Where each argument of a signature lives under AAPCS64 (the guest) and System V (the host).
//!
//! Every type of the signature vocabulary fits one 64-bit slot, and both conventions place such
//! arguments the same way: integers and pointers in the next integer register, floating point in
//! the next vector register, and once a class runs out of registers, the argument takes the next
//! 8-byte stack slot in argument order. Only the register counts differ: AAPCS64 has 8 of each,
//! System V 6 integer and 8 vector registers. Variadic arguments follow the same rules on Linux
//! AAPCS64, unlike Apple's.

use crate::table::Type;

pub const GUEST_INTEGER_REGISTERS: usize = 8;
pub const GUEST_VECTOR_REGISTERS: usize = 8;
pub const HOST_INTEGER_REGISTERS: usize = 6;
pub const HOST_VECTOR_REGISTERS: usize = 8;

/// The register class of an argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Integer,
    Vector,
}

impl Class {
    /// `None` for `void` and `va_list`, which no argument slot can hold as a value.
    pub fn of(kind: Type) -> Option<Class> {
        match kind {
            Type::Void | Type::VaList => None,
            Type::F32 | Type::F64 => Some(Class::Vector),
            _ => Some(Class::Integer),
        }
    }
}

/// Where one argument is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The n-th integer register: x0.. for the guest, rdi, rsi, rdx, rcx, r8, r9 for the host.
    Integer(usize),
    /// The n-th vector register: v0.. or xmm0...
    Vector(usize),
    /// The n-th 8-byte slot from the stack pointer at the call.
    Stack(usize),
}

/// The slots of every argument, in order, and how many stack slots they use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub slots: Vec<Slot>,
    pub stack_slots: usize,
}

pub fn layout(classes: &[Class], integer_registers: usize, vector_registers: usize) -> Layout {
    let (mut integers, mut vectors, mut stack) = (0, 0, 0);
    let slots = classes
        .iter()
        .map(|class| {
            let (used, limit, register): (&mut usize, usize, fn(usize) -> Slot) = match class {
                Class::Integer => (&mut integers, integer_registers, Slot::Integer),
                Class::Vector => (&mut vectors, vector_registers, Slot::Vector),
            };
            if *used < limit {
                *used += 1;
                register(*used - 1)
            } else {
                stack += 1;
                Slot::Stack(stack - 1)
            }
        })
        .collect();
    Layout {
        slots,
        stack_slots: stack,
    }
}

pub fn guest_layout(classes: &[Class]) -> Layout {
    layout(classes, GUEST_INTEGER_REGISTERS, GUEST_VECTOR_REGISTERS)
}

pub fn host_layout(classes: &[Class]) -> Layout {
    layout(classes, HOST_INTEGER_REGISTERS, HOST_VECTOR_REGISTERS)
}

/// A raw 64-bit argument or result normalized for its type: narrow integers extended as the
/// receiving convention expects (neither leaves their upper bits defined), `f32` in the low half
/// with zeroes above.
pub fn normalize(kind: Type, raw: u64) -> u64 {
    match kind {
        Type::I8 => raw as i8 as i64 as u64,
        Type::U8 => u64::from(raw as u8),
        Type::I16 => raw as i16 as i64 as u64,
        Type::U16 => u64::from(raw as u16),
        Type::I32 => raw as i32 as i64 as u64,
        Type::U32 | Type::F32 => u64::from(raw as u32),
        _ => raw,
    }
}

/// A guest `va_list`, AAPCS64's `__va_list`: the next stack argument, the tops of the general and
/// vector register save areas, and negative offsets from each top while saved registers remain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestVaList {
    pub stack: u64,
    pub gr_top: u64,
    pub vr_top: u64,
    pub gr_offs: i32,
    pub vr_offs: i32,
}

impl GuestVaList {
    /// From the struct's four little-endian words.
    pub fn from_words(words: [u64; 4]) -> GuestVaList {
        GuestVaList {
            stack: words[0],
            gr_top: words[1],
            vr_top: words[2],
            gr_offs: words[3] as u32 as i32,
            vr_offs: (words[3] >> 32) as u32 as i32,
        }
    }

    /// Where the next argument of `class` is, as `va_arg` finds it, and moves past it. Every type
    /// the bridge reads is 8 bytes: integers and pointers, and doubles (a variadic float is
    /// promoted), the low half of a 16-byte vector slot.
    pub fn next(&mut self, class: Class) -> u64 {
        let (offset, top, step) = match class {
            Class::Integer => (&mut self.gr_offs, self.gr_top, 8),
            Class::Vector => (&mut self.vr_offs, self.vr_top, 16),
        };
        if *offset < 0 {
            let at = *offset;
            *offset = at.saturating_add(step);
            if *offset <= 0 {
                return top.wrapping_add_signed(i64::from(at));
            }
        }
        let address = self.stack;
        self.stack = self.stack.wrapping_add(8);
        address
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Class::{Integer as I, Vector as V};

    #[test]
    fn registers_fill_per_class_then_the_stack_in_argument_order() {
        // f(int, double, int, ..., 9 doubles): the 9th double and the 7th and 8th ints overflow.
        let classes = [I, V, I, I, I, I, I, I, I, V, V, V, V, V, V, V, V];
        let guest = guest_layout(&classes);
        assert_eq!(
            guest.slots[..3],
            [Slot::Integer(0), Slot::Vector(0), Slot::Integer(1)]
        );
        assert_eq!(guest.slots[8], Slot::Integer(7));
        assert_eq!(guest.slots[16], Slot::Stack(0));
        assert_eq!(guest.stack_slots, 1);

        let host = host_layout(&classes);
        assert_eq!(host.slots[6], Slot::Integer(5));
        assert_eq!(host.slots[7], Slot::Stack(0));
        assert_eq!(host.slots[8], Slot::Stack(1));
        assert_eq!(host.slots[15], Slot::Vector(7));
        assert_eq!(host.slots[16], Slot::Stack(2));
        assert_eq!(host.stack_slots, 3);
    }

    #[test]
    fn narrow_values_are_extended_by_type() {
        assert_eq!(normalize(Type::I8, 0xFFFF_FF80), u64::MAX - 0x7F);
        assert_eq!(normalize(Type::U8, 0x1234_5680), 0x80);
        assert_eq!(normalize(Type::I16, 0x8000), 0xFFFF_FFFF_FFFF_8000);
        assert_eq!(normalize(Type::I32, 0xDEAD_0000_FFFF_FFFF), u64::MAX);
        assert_eq!(normalize(Type::U32, 0xDEAD_0000_FFFF_FFFF), 0xFFFF_FFFF);
        assert_eq!(normalize(Type::F32, 0xDEAD_BEEF_3F80_0000), 0x3F80_0000);
        assert_eq!(
            normalize(Type::Ptr, 0xDEAD_BEEF_3F80_0000),
            0xDEAD_BEEF_3F80_0000
        );
    }

    #[test]
    fn void_and_va_list_have_no_slot() {
        assert_eq!(Class::of(Type::Void), None);
        assert_eq!(Class::of(Type::VaList), None);
        assert_eq!(Class::of(Type::F32), Some(V));
        assert_eq!(Class::of(Type::U16), Some(I));
    }

    #[test]
    fn a_va_list_takes_saved_registers_then_the_stack_per_class() {
        // Two general and one vector register left, as after `f(a, b, c, d, e, f, ...)` with
        // one double already read.
        let mut list = GuestVaList::from_words([
            0x9000,
            0x8000,
            0x7000,
            (-32i64 as u64) << 32 | (-16i32 as u32 as u64),
        ]);
        assert_eq!(list.gr_offs, -16);
        assert_eq!(list.vr_offs, -32);
        assert_eq!(list.next(I), 0x8000 - 16);
        assert_eq!(list.next(V), 0x7000 - 32);
        assert_eq!(list.next(I), 0x8000 - 8);
        assert_eq!(list.next(V), 0x7000 - 16);
        // Both save areas are used up: the stack, in argument order, 8 bytes each.
        assert_eq!(list.next(I), 0x9000);
        assert_eq!(list.next(V), 0x9008);
        assert_eq!(list.next(I), 0x9010);
        assert_eq!(list.gr_offs, 0);
    }
}
