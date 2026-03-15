pub const WORD_BYTES: usize = core::mem::size_of::<usize>();
pub const ALIGNMENT: usize = 8;

pub const FIXNUM_TAG: usize = 0b001;
pub const FIXNUM_SHIFT: usize = 1;
pub const IMMEDIATE_TAG_MASK: usize = 0b111;
pub const HEAP_REF_TAG: usize = 0b000;

pub const BOOL_FALSE: usize = 0b0000_0010;
pub const BOOL_TRUE: usize = 0b0000_0110;
pub const EMPTY_LIST: usize = 0b0000_1010;
pub const UNSPECIFIED: usize = 0b0000_1110;
pub const TAIL_CALL_MARKER: usize = 0b0001_0010;
pub const CHAR_TAG: usize = 0b100;
pub const CHAR_SHIFT: usize = 3;

pub const HEADER_TAG_PAIR: u16 = 1;
pub const HEADER_TAG_VECTOR: u16 = 2;
pub const HEADER_TAG_STRING: u16 = 3;
pub const HEADER_TAG_SYMBOL: u16 = 4;
pub const HEADER_TAG_CLOSURE: u16 = 5;
pub const HEADER_TAG_BOX: u16 = 6;
pub const HEADER_TAG_VALUES: u16 = 7;
pub const HEADER_TAG_PROMISE: u16 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ObjectHeader {
    pub metadata: usize,
    pub kind: u16,
    pub flags: u16,
    pub bytes: u32,
}

impl ObjectHeader {
    pub const fn new(kind: u16, bytes: u32) -> Self {
        Self {
            metadata: 0,
            kind,
            flags: 0,
            bytes,
        }
    }
}
