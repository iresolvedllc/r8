//! Keel: the versioned C ABI shared by the Rust language layer and the Zig
//! machine layer.

pub const ABI_VERSION: u32 = 1;

pub const TAG_INT: u32 = 1;
pub const TAG_OBJECT: u32 = 2;

pub const OP_LOAD_INT: u32 = 0;
pub const OP_ADD: u32 = 1;
pub const OP_RETURN: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Value {
    pub tag: u32,
    pub _pad: u32,
    pub payload: i64,
}

impl Value {
    pub const fn int(value: i64) -> Self {
        Self {
            tag: TAG_INT,
            _pad: 0,
            payload: value,
        }
    }

    pub fn as_int(self) -> Option<i64> {
        (self.tag == TAG_INT).then_some(self.payload)
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Instruction {
    pub op: u32,
    pub operand: i32,
}

impl Instruction {
    pub const fn load_int(value: i64) -> Self {
        Self {
            op: OP_LOAD_INT,
            operand: value as i32,
        }
    }

    pub const fn add() -> Self {
        Self {
            op: OP_ADD,
            operand: 0,
        }
    }

    pub const fn return_() -> Self {
        Self {
            op: OP_RETURN,
            operand: 0,
        }
    }
}

unsafe extern "C" {
    pub fn r8_abi_version() -> u32;
    pub fn r8_machine_run(instructions: *const Instruction, len: usize) -> Value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_stable() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(std::mem::align_of::<Value>(), 8);
        assert_eq!(std::mem::size_of::<Instruction>(), 8);
        assert_eq!(std::mem::align_of::<Instruction>(), 4);
    }

    #[test]
    fn int_round_trip() {
        assert_eq!(Value::int(42).as_int(), Some(42));
        assert_eq!(Value::int(-7).as_int(), Some(-7));
    }
}
