use std::{error::Error as StdError, fmt};

/// The stupidest box error ever. It's not even Send.
///
/// It has `From` implementations for some libstd error types,
/// you can derive `From<E>` for your own error types
/// with [make_bxable!]
pub struct BX(Box<dyn StdError>);

/// A type alias where `E` defaults to `BX`.
pub type Result<T, E = BX> = std::result::Result<T, E>;

impl BX {
    /// Create a new `BX` from an `E`.
    pub fn from_err(e: impl StdError + 'static) -> Self {
        Self(e.into())
    }

    /// Create a new `BX` from a boxed `E`.
    pub fn from_boxed(e: Box<dyn StdError + 'static>) -> Self {
        Self(e)
    }

    /// Create a new `BX` from a String
    pub fn from_string(s: String) -> Self {
        Self(Box::new(StringError(s)))
    }
}

pub fn box_error(e: impl StdError + 'static) -> BX {
    BX::from_err(e)
}

/// Adds `bx() -> BX` to error types
pub trait BxForErrors {
    fn bx(self) -> BX;
}

impl<E: StdError + 'static> BxForErrors for E {
    fn bx(self) -> BX {
        BX::from_err(self)
    }
}

/// Adds `bx() -> Result<T, BX>` to result types
pub trait BxForResults<T> {
    fn bx(self) -> Result<T, BX>;
}

impl<T, E: StdError + 'static> BxForResults<T> for Result<T, E> {
    fn bx(self) -> Result<T, BX> {
        self.map_err(BX::from_err)
    }
}

impl fmt::Debug for BX {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for BX {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl StdError for BX {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0.source()
    }
}

/// Implements `From<E>` for `BX` for your own type.
#[macro_export]
macro_rules! make_bxable {
    ($ty:ty) => {
        impl From<$ty> for $crate::BX {
            fn from(e: $ty) -> Self {
                $crate::BX::from_err(e)
            }
        }
    };
}

make_bxable!(std::io::Error);
make_bxable!(std::fmt::Error);
make_bxable!(std::str::Utf8Error);
make_bxable!(std::string::FromUtf8Error);
make_bxable!(std::string::FromUtf16Error);
make_bxable!(std::num::ParseIntError);
make_bxable!(std::num::ParseFloatError);
make_bxable!(std::num::TryFromIntError);
make_bxable!(std::array::TryFromSliceError);
make_bxable!(std::char::ParseCharError);
make_bxable!(std::net::AddrParseError);
make_bxable!(std::time::SystemTimeError);
make_bxable!(std::env::VarError);
make_bxable!(std::sync::mpsc::RecvError);
make_bxable!(std::sync::mpsc::TryRecvError);
make_bxable!(std::sync::mpsc::SendError<Box<dyn StdError + Send + Sync>>);
make_bxable!(std::sync::PoisonError<Box<dyn StdError + Send + Sync>>);

#[macro_export]
macro_rules! bail {
    ($err:expr) => {
        return Err($crate::BX::from_err($err));
    };
    ($fmt:expr, $($arg:tt)*) => {
        return Err($crate::BX::from_string(format!($fmt, $($arg)*)));
    };
}

/// The stupidest Send box error ever.
///
/// It has `From` implementations for some libstd error types,
/// you can derive `From<E>` for your own error types
pub struct BS(Box<dyn StdError + Send>);

#[derive(Debug)]
struct StringError(String);

impl fmt::Display for StringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl StdError for StringError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        None
    }
}

impl BS {
    /// Create a new `BS` from an `E`.
    pub fn from_err(e: impl StdError + Send + 'static) -> Self {
        Self(Box::new(e))
    }

    /// Create a new `BS` from a boxed `E`.
    pub fn from_boxed(e: Box<dyn StdError + Send + 'static>) -> Self {
        Self(e)
    }

    /// Create a new `BS` from a String
    pub fn from_string(s: String) -> Self {
        Self(Box::new(StringError(s)))
    }
}

pub fn box_send_error(e: impl StdError + Send + 'static) -> BS {
    BS::from_err(e)
}

/// Adds `bs() -> BS` to error types
pub trait BsForErrors {
    fn bs(self) -> BS;
}

impl<E: StdError + Send + 'static> BsForErrors for E {
    fn bs(self) -> BS {
        BS::from_err(self)
    }
}

/// Adds `bs() -> Result<T, BS>` to result types
pub trait BsForResults<T> {
    fn bs(self) -> Result<T, BS>;
}

impl<T, E: StdError + Send + 'static> BsForResults<T> for Result<T, E> {
    fn bs(self) -> Result<T, BS> {
        self.map_err(BS::from_err)
    }
}

impl fmt::Debug for BS {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for BS {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl StdError for BS {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0.source()
    }
}

#[macro_export]
macro_rules! make_bsable {
    ($ty:ty) => {
        impl From<$ty> for $crate::BS {
            fn from(e: $ty) -> Self {
                $crate::BS::from_err(e)
            }
        }
    };
}

make_bsable!(std::io::Error);
make_bsable!(std::fmt::Error);
make_bsable!(std::str::Utf8Error);
make_bsable!(std::string::FromUtf8Error);
make_bsable!(std::string::FromUtf16Error);
make_bsable!(std::num::ParseIntError);
make_bsable!(std::num::ParseFloatError);
make_bsable!(std::num::TryFromIntError);
make_bsable!(std::array::TryFromSliceError);
make_bsable!(std::char::ParseCharError);
make_bsable!(std::net::AddrParseError);
make_bsable!(std::time::SystemTimeError);
make_bsable!(std::env::VarError);
make_bsable!(std::sync::mpsc::RecvError);
make_bsable!(std::sync::mpsc::TryRecvError);
make_bsable!(std::sync::mpsc::SendError<Box<dyn StdError + Send + Sync>>);
make_bsable!(std::sync::PoisonError<Box<dyn StdError + Send + Sync>>);
