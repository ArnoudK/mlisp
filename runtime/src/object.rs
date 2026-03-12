use crate::layout::{
    HEADER_TAG_BOX, HEADER_TAG_CLOSURE, HEADER_TAG_PAIR, HEADER_TAG_STRING, HEADER_TAG_SYMBOL,
    HEADER_TAG_VECTOR, ObjectHeader,
};
use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapKind {
    Pair,
    Vector,
    String,
    Symbol,
    Closure,
    Box,
}

impl HeapKind {
    pub const fn as_tag(self) -> u16 {
        match self {
            Self::Pair => HEADER_TAG_PAIR,
            Self::Vector => HEADER_TAG_VECTOR,
            Self::String => HEADER_TAG_STRING,
            Self::Symbol => HEADER_TAG_SYMBOL,
            Self::Closure => HEADER_TAG_CLOSURE,
            Self::Box => HEADER_TAG_BOX,
        }
    }
}

#[repr(C)]
pub struct PairObject {
    pub header: ObjectHeader,
    pub car: usize,
    pub cdr: usize,
}

impl PairObject {
    pub fn new(car: Value, cdr: Value) -> Self {
        Self {
            header: ObjectHeader::new(HeapKind::Pair.as_tag(), core::mem::size_of::<Self>() as u32),
            car: car.bits(),
            cdr: cdr.bits(),
        }
    }

    pub fn car(&self) -> Value {
        Value::from_bits(self.car)
    }

    pub fn cdr(&self) -> Value {
        Value::from_bits(self.cdr)
    }
}

#[repr(C)]
pub struct BoxObject {
    pub header: ObjectHeader,
    pub value: usize,
}

impl BoxObject {
    pub fn new(value: Value) -> Self {
        Self {
            header: ObjectHeader::new(HeapKind::Box.as_tag(), core::mem::size_of::<Self>() as u32),
            value: value.bits(),
        }
    }

    pub fn value(&self) -> Value {
        Value::from_bits(self.value)
    }
}

#[repr(C)]
pub struct ClosureObject {
    pub header: ObjectHeader,
    pub code_ptr: usize,
    pub env_len: usize,
}

impl ClosureObject {
    pub fn new(code_ptr: usize, env_len: usize, total_bytes: usize) -> Self {
        Self {
            header: ObjectHeader::new(
                HeapKind::Closure.as_tag(),
                total_bytes as u32,
            ),
            code_ptr,
            env_len,
        }
    }

    pub fn env_ptr(&self) -> *const usize {
        unsafe { (self as *const Self).cast::<usize>().add(core::mem::size_of::<Self>() / core::mem::size_of::<usize>()) }
    }

    pub fn env_mut_ptr(&mut self) -> *mut usize {
        unsafe { (self as *mut Self).cast::<usize>().add(core::mem::size_of::<Self>() / core::mem::size_of::<usize>()) }
    }
}

#[repr(C)]
pub struct StringObject {
    pub header: ObjectHeader,
    pub length: usize,
}

impl StringObject {
    pub fn new(length: usize, total_bytes: usize) -> Self {
        Self {
            header: ObjectHeader::new(HeapKind::String.as_tag(), total_bytes as u32),
            length,
        }
    }

    pub fn bytes_ptr(&self) -> *const u8 {
        unsafe { (self as *const Self).cast::<u8>().add(core::mem::size_of::<Self>()) }
    }

    pub fn bytes_mut_ptr(&mut self) -> *mut u8 {
        unsafe { (self as *mut Self).cast::<u8>().add(core::mem::size_of::<Self>()) }
    }
}

#[repr(C)]
pub struct SymbolObject {
    pub header: ObjectHeader,
    pub length: usize,
}

impl SymbolObject {
    pub fn new(length: usize, total_bytes: usize) -> Self {
        Self {
            header: ObjectHeader::new(HeapKind::Symbol.as_tag(), total_bytes as u32),
            length,
        }
    }

    pub fn bytes_ptr(&self) -> *const u8 {
        unsafe { (self as *const Self).cast::<u8>().add(core::mem::size_of::<Self>()) }
    }

    pub fn bytes_mut_ptr(&mut self) -> *mut u8 {
        unsafe { (self as *mut Self).cast::<u8>().add(core::mem::size_of::<Self>()) }
    }
}

#[repr(C)]
pub struct VectorObject {
    pub header: ObjectHeader,
    pub length: usize,
}

impl VectorObject {
    pub fn new(length: usize, total_bytes: usize) -> Self {
        Self {
            header: ObjectHeader::new(HeapKind::Vector.as_tag(), total_bytes as u32),
            length,
        }
    }

    pub fn elements_ptr(&self) -> *const usize {
        unsafe { (self as *const Self).cast::<usize>().add(core::mem::size_of::<Self>() / core::mem::size_of::<usize>()) }
    }

    pub fn elements_mut_ptr(&mut self) -> *mut usize {
        unsafe { (self as *mut Self).cast::<usize>().add(core::mem::size_of::<Self>() / core::mem::size_of::<usize>()) }
    }
}
