use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use serde_json::Value;

/// A validated data-size threshold, bounded by the lifecycle contract's 5 TiB limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LifecycleSize(u64);

impl LifecycleSize {
    pub(crate) fn bytes(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for LifecycleSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let bytes = crate::serde_util::int64(&value)
            .filter(|bytes| (0..=5 * 1024_i64.pow(4)).contains(bytes))
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(|| D::Error::custom("lifecycle size must be an int64 from 0 to 5 TiB"))?;
        Ok(Self(bytes))
    }
}

impl Serialize for LifecycleSize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use crate::bucket::lifecycle::{LifecycleRule, parse};
    use serde_json::json;

    #[test]
    fn lifecycle_size_accepts_int64_strings_and_numbers_through_five_tebibytes() {
        for field in ["sizeAboveBytes", "sizeBelowBytes"] {
            for bytes in [0, 1, 5 * 1024_i64.pow(4)] {
                for value in [json!(bytes), json!(bytes.to_string())] {
                    let condition = json!({field:value});
                    let stored = parse(
                        &json!({"rule":[{"action":{"type":"Delete"},"condition":condition}]}),
                    )
                    .expect("valid size configuration");
                    assert_eq!(stored[0]["condition"][field], bytes.to_string());
                    let typed = LifecycleRule::from_stored(&stored).expect("valid stored rule");
                    let size = typed[0]
                        .condition
                        .size_above_bytes
                        .or(typed[0].condition.size_below_bytes)
                        .expect("size condition");
                    assert_eq!(
                        size.bytes(),
                        u64::try_from(bytes).expect("nonnegative test size")
                    );
                    let encoded = LifecycleRule::to_stored(&typed).expect("valid typed rule");
                    assert_eq!(encoded[0]["condition"][field], bytes.to_string());
                    assert_eq!(encoded, stored);
                }
            }
        }
    }

    #[test]
    fn lifecycle_size_normalizes_decimal_spelling_before_storage() {
        for (input, expected) in [("+1", "1"), ("007", "7"), ("-0", "0")] {
            let stored = parse(&json!({"rule":[{"action":{"type":"Delete"},"condition":{"sizeAboveBytes":input,"sizeBelowBytes":input}}]})).expect("valid decimal string");
            assert_eq!(
                stored[0]["condition"],
                json!({"sizeAboveBytes":expected,"sizeBelowBytes":expected})
            );
            let typed = LifecycleRule::from_stored(&stored).expect("canonical rule");
            assert_eq!(LifecycleRule::to_stored(&typed).expect("roundtrip"), stored);
        }
    }

    #[test]
    fn lifecycle_size_rejects_out_of_range_or_non_int64_values() {
        for field in ["sizeAboveBytes", "sizeBelowBytes"] {
            for value in [
                json!(-1),
                json!("-1"),
                json!(5 * 1024_i64.pow(4) + 1),
                json!("5497558138881"),
                json!(i64::MAX),
                json!("9223372036854775808"),
                json!(u64::MAX),
                json!(1.5),
                json!("1.5"),
                json!(true),
                json!([]),
                json!({}),
                json!(""),
            ] {
                assert!(
                    parse(
                        &json!({"rule":[{"action":{"type":"Delete"},"condition":{field:value}}]})
                    )
                    .is_err(),
                    "{field}: {value}"
                );
            }
        }
    }

    #[test]
    fn lifecycle_size_nulls_normalize_but_abort_and_pattern_remain_rejected() {
        let stored = parse(&json!({"rule":[{"action":{"type":"Delete"},"condition":{"age":1,"sizeAboveBytes":null,"sizeBelowBytes":null}}]})).expect("null optional fields");
        assert_eq!(stored[0]["condition"], json!({"age":1}));
        assert!(
            parse(
                &json!({"rule":[{"action":{"type":"Delete"},"condition":{"sizeAboveBytes":null}}]})
            )
            .is_err()
        );
        for field in ["sizeAboveBytes", "sizeBelowBytes"] {
            assert!(parse(&json!({"rule":[{"action":{"type":"AbortIncompleteMultipartUpload"},"condition":{field:1}}]})).is_err());
        }
        assert!(matches!(
            parse(
                &json!({"rule":[{"action":{"type":"Delete"},"condition":{"matchesPattern":".*"}}]})
            ),
            Err(crate::bucket::CatalogError::UnsupportedMetadata(_))
        ));
    }
}
