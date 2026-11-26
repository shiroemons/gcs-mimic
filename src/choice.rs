//! Shared spelling tables for configuration, wire values and stored text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidChoice(pub(crate) &'static str);

impl std::fmt::Display for InvalidChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for InvalidChoice {}

/// An enum spelled by fixed strings, with `as_str` and an exact-match `FromStr`.
///
/// An optional `; withdrawn { "old" => "fix" }` after the error lists values a release removed or
/// renamed: parsing one fails with its own fix, which names the replacement, instead of the
/// generic error. The clause follows the error because an expression may start with any
/// identifier, so a leading keyword could not be told apart from it.
macro_rules! string_enum {
    // `listed` also lists every variant, for an enum whose every value a text such as the help
    // must name.
    (no_default listed $(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)?
    } $error:expr $(; withdrawn { $($old:literal => $fix:literal),+ $(,)? })?) => {
        $crate::choice::string_enum! { @impl [] $(#[$meta])* $vis enum $name {
            $($(#[$variant_meta])* $variant => $value),+
        } $error; [$($($old => $fix),+)?] }

        impl $name {
            /// Every variant, in declaration order.
            $vis const ALL: &'static [Self] = &[$(Self::$variant),+];
        }
    };
    ($(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)?
    } $error:expr $(; withdrawn { $($old:literal => $fix:literal),+ $(,)? })?) => {
        $crate::choice::string_enum! { @impl [Default] $(#[$meta])* $vis enum $name {
            $($(#[$variant_meta])* $variant => $value),+
        } $error; [$($($old => $fix),+)?] }
    };
    (no_default $(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)?
    } $error:expr $(; withdrawn { $($old:literal => $fix:literal),+ $(,)? })?) => {
        $crate::choice::string_enum! { @impl [] $(#[$meta])* $vis enum $name {
            $($(#[$variant_meta])* $variant => $value),+
        } $error; [$($($old => $fix),+)?] }
    };
    (@impl [$($derive:ident),*] $(#[$meta:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)?
    } $error:expr; [$($old:literal => $fix:literal),*]) => {
        $crate::choice::string_enum! {
            @enum $(#[$meta])*
            #[derive(Clone, Copy, Debug, PartialEq, Eq, $($derive),*)]
            $vis enum $name as as_str {
                $($(#[$variant_meta])* $variant => $value),+
            };
            from_str($crate::choice::InvalidChoice,
                $($old => Err($crate::choice::InvalidChoice($fix)),)*
                _ => Err($crate::choice::InvalidChoice($error)))
        }
    };
    // A spelling table with the implementations its callers need.
    (@enum $(#[$meta:meta])* $vis:vis enum $name:ident as $method:ident {
        $($(#[$variant_meta:meta])* $variant:ident => $value:tt $(| $alias:literal)*),+ $(,)?
    } $(; $option:ident $args:tt)*) => {
        $(#[$meta])*
        $vis enum $name { $($(#[$variant_meta])* $variant),+ }

        impl $name {
            $vis const fn $method(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }

        $crate::choice::string_enum! {
            @options $name [$($variant => $value $(| $alias)*),+] $(; $option $args)*
        }
    };
    (@options $name:ident $table:tt) => {};
    (@options $name:ident $table:tt; $option:ident $args:tt $($rest:tt)*) => {
        $crate::choice::string_enum! { @option $option $name $table $args }
        $crate::choice::string_enum! { @options $name $table $($rest)* }
    };
    (@option listed $name:ident [$($variant:ident => $value:tt $(| $alias:literal)*),+]
        ($(#[$meta:meta])* $vis:vis)
    ) => {
        impl $name {
            $(#[$meta])*
            $vis const ALL: [Self; [$($name::$variant),+].len()] = [$(Self::$variant),+];
        }
    };
    (@option from_str $name:ident $table:tt ($error:ty, $($invalid:tt)+)) => {
        impl std::str::FromStr for $name {
            type Err = $error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                $crate::choice::string_enum! { @parse $table value, Ok, $($invalid)+ }
            }
        }
    };
    (@option parse $name:ident $table:tt ($vis:vis $method:ident, $error:ty, $($invalid:tt)+)) => {
        impl $name {
            $vis fn $method(value: &str) -> Result<Self, $error> {
                $crate::choice::string_enum! { @parse $table value, Ok, $($invalid)+ }
            }
        }
    };
    (@parse [$($variant:ident => $value:tt $(| $alias:literal)*),+]
        $input:ident, $valid:ident, $($invalid:tt)+
    ) => {
        match $input {
            $($value $(| $alias)* => $valid(Self::$variant),)+
            $($invalid)+
        }
    };
    (@option ascii_case $name:ident [$($variant:ident => $value:tt),+] ($error:ty, $invalid:expr)) => {
        impl std::str::FromStr for $name {
            type Err = $error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                [$((Self::$variant, $value)),+]
                    .into_iter()
                    .find_map(|(variant, spelling)| value.eq_ignore_ascii_case(spelling).then_some(variant))
                    .ok_or($invalid)
            }
        }
    };
    (@option serde $name:ident $table:tt ()) => {
        $crate::choice::string_enum! { @serde $name }
    };
    (@option serde $name:ident $table:tt (try_from_string)) => {
        $crate::choice::string_enum! { @serde $name }

        impl TryFrom<String> for $name {
            type Error = <Self as std::str::FromStr>::Err;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                value.parse()
            }
        }
    };
    (@serde $name:ty) => {
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
    };
    (@option sql $name:ident $table:tt ($vis:vis, $invalid:literal)) => {
        impl $name {
            $vis fn parse(value: &str) -> Option<Self> {
                $crate::choice::string_enum! { @parse $table value, Some, _ => None }
            }
        }
        $crate::choice::string_enum! { @sql $name, $invalid, Self::parse }
    };
    (@sql $name:ty, $invalid:literal, $parse:expr) => {
        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::Borrowed(self.as_str().into()))
            }
        }
        impl rusqlite::types::FromSql for $name {
            fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                ($parse)(value.as_str()?).ok_or_else(|| rusqlite::types::FromSqlError::Other($invalid.into()))
            }
        }
    };
}
pub(crate) use string_enum;
