//! Random identifiers that the catalog stores and clients echo back.
//!
//! Every form a person or client sees (`Display`, `FromStr`, serde) is lowercase hexadecimal.
//! Fixed-length lowercase hex sorts like the bytes it encodes, so the derived `Ord` agrees with
//! SQLite's `ORDER BY` on the stored TEXT and a keyset cursor may compare either form.

use std::fmt;

/// Text that is not the canonical lowercase hexadecimal form of an identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidOpaqueId {
    what: &'static str,
}

impl InvalidOpaqueId {
    pub(crate) const fn new(what: &'static str) -> Self {
        Self { what }
    }
}

impl fmt::Display for InvalidOpaqueId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.what)
    }
}

impl std::error::Error for InvalidOpaqueId {}

/// `N` bytes from the operating system's random source.
pub(crate) fn random<const N: usize>() -> Result<[u8; N], getrandom::Error> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes)?;
    Ok(bytes)
}

/// Defines a fixed-length random identifier. `column = text` stores the lowercase hexadecimal
/// form in a TEXT column; `column = blob` stores the raw bytes in a BLOB column.
macro_rules! opaque_id {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident([u8; $len:literal]);
        invalid = $what:literal;
        column = $column:ident;
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis struct $name([u8; $len]);

        impl $name {
            /// The lowercase hexadecimal form, as stored in TEXT columns and shown to clients.
            $vis fn to_hex(self) -> String {
                $crate::hashing::encode_lower(&self.0)
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::storage::opaque_id::InvalidOpaqueId;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                $crate::hex::decode_lower(text)
                    .map(Self)
                    .ok_or($crate::storage::opaque_id::InvalidOpaqueId::new($what))
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                formatter.write_str(&self.to_hex())
            }
        }

        impl ::std::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&format_args!("{self}"))
                    .finish()
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(::serde::de::Error::custom)
            }
        }

        $crate::storage::opaque_id::opaque_id!(@column $name, $len, $column);
    };
    (@column $name:ident, $len:literal, text) => {
        impl ::rusqlite::ToSql for $name {
            fn to_sql(&self) -> ::rusqlite::Result<::rusqlite::types::ToSqlOutput<'_>> {
                Ok(::rusqlite::types::ToSqlOutput::Owned(self.to_hex().into()))
            }
        }

        impl ::rusqlite::types::FromSql for $name {
            fn column_result(
                value: ::rusqlite::types::ValueRef<'_>,
            ) -> ::rusqlite::types::FromSqlResult<Self> {
                value
                    .as_str()?
                    .parse()
                    .map_err(|error| ::rusqlite::types::FromSqlError::Other(Box::new(error)))
            }
        }
    };
    (@column $name:ident, $len:literal, blob) => {
        impl ::rusqlite::ToSql for $name {
            fn to_sql(&self) -> ::rusqlite::Result<::rusqlite::types::ToSqlOutput<'_>> {
                Ok(::rusqlite::types::ToSqlOutput::Borrowed(
                    ::rusqlite::types::ValueRef::Blob(&self.0),
                ))
            }
        }

        impl ::rusqlite::types::FromSql for $name {
            fn column_result(
                value: ::rusqlite::types::ValueRef<'_>,
            ) -> ::rusqlite::types::FromSqlResult<Self> {
                <[u8; $len] as ::rusqlite::types::FromSql>::column_result(value).map(Self)
            }
        }
    };
}
pub(crate) use opaque_id;
