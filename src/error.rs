use std::fmt::{Display, Formatter};
use std::path::PathBuf;

#[derive(Debug)]
pub enum CompileError {
    Io {
        path: Option<PathBuf>,
        message: String,
    },
    Usage(String),
    Lex(String),
    Parse(String),
    Lower(String),
    Codegen(String),
    Thread(String),
}

impl CompileError {
    pub fn io(path: impl Into<Option<PathBuf>>, error: impl std::error::Error) -> Self {
        Self::Io {
            path: path.into(),
            message: error.to_string(),
        }
    }
}

impl Display for CompileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, message } => match path {
                Some(path) => write!(f, "io error for {}: {message}", path.display()),
                None => write!(f, "io error: {message}"),
            },
            Self::Usage(message) => write!(f, "usage error: {message}"),
            Self::Lex(message) => write!(f, "lex error: {message}"),
            Self::Parse(message) => write!(f, "parse error: {message}"),
            Self::Lower(message) => write!(f, "lowering error: {message}"),
            Self::Codegen(message) => write!(f, "codegen error: {message}"),
            Self::Thread(message) => write!(f, "thread error: {message}"),
        }
    }
}

impl std::error::Error for CompileError {}
