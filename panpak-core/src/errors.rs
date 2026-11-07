// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stable error codes (`PANPAK-Exxxx`), documented in docs/ERRORS.md.
//!
//! Groups: E0 format and index, E1 signature and trust, E2 store and
//! materialization, E3 sandbox and permissions, E4 updates and network,
//! E5 CLI usage and options, E6 builds from source. `Exxx0` of each group is the generic code used
//! when an error carries no specific one.
use std::fmt;

macro_rules! codes {
    ($($name:ident = $s:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorCode { $($name,)* }
        impl ErrorCode {
            /// Every code, in documentation order.
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$name,)*];
            pub const fn as_str(self) -> &'static str {
                match self { $(ErrorCode::$name => $s,)* }
            }
        }
    };
}

codes! {
    E0000 = "PANPAK-E0000",
    E0001 = "PANPAK-E0001",
    E0002 = "PANPAK-E0002",
    E0003 = "PANPAK-E0003",
    E0004 = "PANPAK-E0004",
    E0005 = "PANPAK-E0005",
    E0006 = "PANPAK-E0006",
    E0007 = "PANPAK-E0007",
    E0008 = "PANPAK-E0008",
    E1000 = "PANPAK-E1000",
    E1001 = "PANPAK-E1001",
    E1002 = "PANPAK-E1002",
    E1003 = "PANPAK-E1003",
    E1004 = "PANPAK-E1004",
    E1005 = "PANPAK-E1005",
    E2000 = "PANPAK-E2000",
    E2001 = "PANPAK-E2001",
    E2002 = "PANPAK-E2002",
    E2003 = "PANPAK-E2003",
    E2004 = "PANPAK-E2004",
    E2005 = "PANPAK-E2005",
    E3000 = "PANPAK-E3000",
    E3001 = "PANPAK-E3001",
    E3002 = "PANPAK-E3002",
    E4000 = "PANPAK-E4000",
    E4001 = "PANPAK-E4001",
    E4002 = "PANPAK-E4002",
    E5000 = "PANPAK-E5000",
    E5001 = "PANPAK-E5001",
    E5002 = "PANPAK-E5002",
    E5003 = "PANPAK-E5003",
    E5004 = "PANPAK-E5004",
    E6000 = "PANPAK-E6000",
    E6005 = "PANPAK-E6005",
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error with a stable code; travels inside `anyhow::Error`.
/// Its `Display` is the message only: the code is added when printing.
#[derive(Debug)]
pub struct PanpakError {
    pub code: ErrorCode,
    msg: String,
}

impl fmt::Display for PanpakError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for PanpakError {}

pub fn coded(code: ErrorCode, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(PanpakError { code, msg: msg.into() })
}

/// `bail!` with a code: `bail_code!(ErrorCode::E1001, "...", args)`.
#[macro_export]
macro_rules! bail_code {
    ($code:expr, $($arg:tt)+) => {
        return Err($crate::coded($code, format!($($arg)+)))
    };
}

/// `ensure!` with a code.
#[macro_export]
macro_rules! ensure_code {
    ($cond:expr, $code:expr, $($arg:tt)+) => {
        if !$cond {
            return Err($crate::coded($code, format!($($arg)+)));
        }
    };
}

/// The code of the first typed error in the chain, else `generic`.
pub fn code_of(e: &anyhow::Error, generic: ErrorCode) -> ErrorCode {
    e.chain().find_map(|c| c.downcast_ref::<PanpakError>()).map_or(generic, |p| p.code)
}

/// "PANPAK-Exxxx: message chain", as printed by every binary.
pub fn render(e: &anyhow::Error, generic: ErrorCode) -> String {
    format!("{}: {}", code_of(e, generic), crate::err_text(e))
}
