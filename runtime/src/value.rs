use crate::layout::{
    BOOL_FALSE, BOOL_TRUE, CHAR_SHIFT, CHAR_TAG, EMPTY_LIST, FIXNUM_SHIFT, FIXNUM_TAG, HEAP_REF_TAG,
    IMMEDIATE_TAG_MASK, UNSPECIFIED,
};
use mmtk::util::{Address, ObjectReference};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Value(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Immediate {
    Bool(bool),
    Char(char),
    EmptyList,
    Unspecified,
}

impl Value {
    pub const fn from_bits(bits: usize) -> Self {
        Self(bits)
    }

    pub fn from_object_reference(object: ObjectReference) -> Self {
        Self(object.to_raw_address().as_usize())
    }

    pub const fn bits(self) -> usize {
        self.0
    }

    pub fn encode_fixnum(value: i64) -> Option<Self> {
        if !(Self::fixnum_min()..=Self::fixnum_max()).contains(&value) {
            return None;
        }
        let shifted = (value as isize) << FIXNUM_SHIFT;
        Some(Self((shifted as usize) | FIXNUM_TAG))
    }

    pub const fn encode_bool(value: bool) -> Self {
        if value {
            Self(BOOL_TRUE)
        } else {
            Self(BOOL_FALSE)
        }
    }

    pub const fn empty_list() -> Self {
        Self(EMPTY_LIST)
    }

    pub const fn unspecified() -> Self {
        Self(UNSPECIFIED)
    }

    pub fn encode_char(value: char) -> Self {
        Self(((value as usize) << CHAR_SHIFT) | CHAR_TAG)
    }

    pub const fn is_fixnum(self) -> bool {
        self.0 & FIXNUM_TAG == FIXNUM_TAG
    }

    pub const fn is_heap_ref(self) -> bool {
        self.0 != 0 && (self.0 & IMMEDIATE_TAG_MASK) == HEAP_REF_TAG
    }

    pub fn to_object_reference(self) -> Option<ObjectReference> {
        if !self.is_heap_ref() {
            return None;
        }

        ObjectReference::from_raw_address(unsafe { Address::from_usize(self.0) })
    }

    pub fn decode_fixnum(self) -> Option<i64> {
        if !self.is_fixnum() {
            return None;
        }

        Some((self.0 as isize >> FIXNUM_SHIFT) as i64)
    }

    pub const fn fixnum_max() -> i64 {
        (1i64 << (usize::BITS as usize - FIXNUM_SHIFT - 2)) - 1
    }

    pub const fn fixnum_min() -> i64 {
        -(1i64 << (usize::BITS as usize - FIXNUM_SHIFT - 2))
    }

    pub const fn decode_immediate(self) -> Option<Immediate> {
        if self.0 & IMMEDIATE_TAG_MASK == CHAR_TAG {
            let scalar = (self.0 >> CHAR_SHIFT) as u32;
            return match char::from_u32(scalar) {
                Some(value) => Some(Immediate::Char(value)),
                None => None,
            };
        }
        match self.0 {
            BOOL_FALSE => Some(Immediate::Bool(false)),
            BOOL_TRUE => Some(Immediate::Bool(true)),
            EMPTY_LIST => Some(Immediate::EmptyList),
            UNSPECIFIED => Some(Immediate::Unspecified),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Immediate, Value};

    #[test]
    fn round_trips_fixnums() {
        let value = Value::encode_fixnum(-42).unwrap();
        assert_eq!(value.decode_fixnum(), Some(-42));
    }

    #[test]
    fn rejects_out_of_range_fixnums() {
        assert!(Value::encode_fixnum(Value::fixnum_max()).is_some());
        assert!(Value::encode_fixnum(Value::fixnum_min()).is_some());
        assert!(Value::encode_fixnum(Value::fixnum_max().saturating_add(1)).is_none());
        assert!(Value::encode_fixnum(Value::fixnum_min().saturating_sub(1)).is_none());
    }

    #[test]
    fn identifies_immediates() {
        assert_eq!(
            Value::encode_bool(true).decode_immediate(),
            Some(Immediate::Bool(true))
        );
        assert_eq!(
            Value::empty_list().decode_immediate(),
            Some(Immediate::EmptyList)
        );
        assert_eq!(
            Value::unspecified().decode_immediate(),
            Some(Immediate::Unspecified)
        );
        assert_eq!(
            Value::encode_char('x').decode_immediate(),
            Some(Immediate::Char('x'))
        );
    }
}
