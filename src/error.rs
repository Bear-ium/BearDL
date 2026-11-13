use std::fmt;

/// Application error: a message string. Any `std::error::Error` converts into it,
/// so `?` works everywhere without pulling in an error crate.
#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn msg(m: impl Into<String>) -> Self {
        Self(m.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}

#[macro_export]
macro_rules! bail {
    ($($t:tt)*) => {
        return Err($crate::error::Error(format!($($t)*)))
    };
}

pub trait Context<T> {
    fn context(self, what: &str) -> Result<T>;
}

impl<T, E: fmt::Display> Context<T> for std::result::Result<T, E> {
    fn context(self, what: &str) -> Result<T> {
        self.map_err(|e| Error(format!("{what}: {e}")))
    }
}

impl<T> Context<T> for Option<T> {
    fn context(self, what: &str) -> Result<T> {
        self.ok_or_else(|| Error(what.to_owned()))
    }
}
