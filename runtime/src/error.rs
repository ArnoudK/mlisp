use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    NotInitialized,
    ThreadNotBound,
    InvalidThread,
    InvalidObjectKind,
    IndexOutOfBounds,
    InvalidArgument,
    NullSlot,
    ShadowStackUnderflow,
    AllocationFailed,
    FixnumOutOfRange,
    WorkerThreadPanicked,
    InvalidTrampolineState,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInitialized => f.write_str("runtime is not initialized"),
            Self::ThreadNotBound => f.write_str("current thread is not bound to the runtime"),
            Self::InvalidThread => f.write_str("invalid runtime thread handle"),
            Self::InvalidObjectKind => f.write_str("invalid object kind"),
            Self::IndexOutOfBounds => f.write_str("index out of bounds"),
            Self::InvalidArgument => f.write_str("invalid runtime argument"),
            Self::NullSlot => f.write_str("slot pointer must not be null"),
            Self::ShadowStackUnderflow => f.write_str("shadow stack underflow"),
            Self::AllocationFailed => f.write_str("allocation failed"),
            Self::FixnumOutOfRange => f.write_str("fixnum does not fit in tagged representation"),
            Self::WorkerThreadPanicked => f.write_str("worker thread panicked"),
            Self::InvalidTrampolineState => f.write_str("invalid trampoline state"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl RuntimeError {
    pub fn io_like(_error: impl std::error::Error) -> Self {
        Self::InvalidArgument
    }
}
