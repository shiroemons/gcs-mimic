pub(crate) use crate::mime::OCTET_STREAM as DEFAULT;

/// Defaults a stored type or a wire field that uses the empty string for an omitted value.
pub(crate) fn or_default(value: String) -> String {
    if value.is_empty() {
        DEFAULT.to_owned()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_empty_types_are_replaced() {
        assert_eq!(or_default(String::new()), "application/octet-stream");
        for value in ["image/PNG", "text/plain; charset=utf-8", " "] {
            assert_eq!(or_default(value.to_owned()), value);
        }
    }
}
