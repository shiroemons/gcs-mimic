//! Customer-supplied encryption keys (CSEK), shared by the JSON, XML and storage layers.
//!
//! Contract: <https://docs.cloud.google.com/storage/docs/encryption/customer-supplied-keys>
//! and the JSON/XML status-code references (checked 2026-10-29). Cloud Storage keeps only
//! the SHA-256 fingerprint of a key, so parsing drops the raw key as soon as the fingerprint
//! is verified. Object bytes are stored unencrypted on local disk: at-rest encryption is
//! Google infrastructure that this emulator simulates only through key enforcement.

use hyper::HeaderMap;

/// The only algorithm Cloud Storage accepts for customer-supplied keys.
pub(crate) const ALGORITHM: &str = "AES256";

/// Header prefix carrying the key that encrypts (or decrypts) the addressed object.
pub(crate) const DESTINATION_PREFIX: &str = "x-goog-encryption-";
/// Header prefix carrying the source object's key on copy and rewrite.
pub(crate) const SOURCE_PREFIX: &str = "x-goog-copy-source-encryption-";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CsekError {
    AlgorithmInvalid,
    KeyFormatInvalid,
    KeySha256Invalid,
    KeyIncorrect,
    ResourceIsEncrypted,
    ResourceNotEncrypted,
}

impl CsekError {
    const ALL: [Self; 6] = [
        Self::AlgorithmInvalid,
        Self::KeyFormatInvalid,
        Self::KeySha256Invalid,
        Self::KeyIncorrect,
        Self::ResourceIsEncrypted,
        Self::ResourceNotEncrypted,
    ];

    /// JSON API error reason; every CSEK failure is a 400 in both APIs.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::AlgorithmInvalid => "customerEncryptionAlgorithmIsInvalid",
            Self::KeyFormatInvalid => "customerEncryptionKeyFormatIsInvalid",
            Self::KeySha256Invalid => "customerEncryptionKeySha256IsInvalid",
            Self::KeyIncorrect => "customerEncryptionKeyIsIncorrect",
            Self::ResourceIsEncrypted => "resourceIsEncryptedWithCustomerEncryptionKey",
            Self::ResourceNotEncrypted => "resourceNotEncryptedWithCustomerEncryptionKey",
        }
    }

    /// XML API error code for the same condition.
    pub(crate) fn xml_code(self) -> &'static str {
        match self {
            Self::AlgorithmInvalid => "CustomerEncryptionAlgorithmIsInvalid",
            Self::KeyFormatInvalid => "CustomerEncryptionKeyFormatIsInvalid",
            Self::KeySha256Invalid => "CustomerEncryptionKeySha256IsInvalid",
            Self::KeyIncorrect => "CustomerEncryptionKeyIsIncorrect",
            Self::ResourceIsEncrypted => "ResourceIsEncryptedWithCustomerEncryptionKey",
            Self::ResourceNotEncrypted => "ResourceNotEncryptedWithCustomerEncryptionKey",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::AlgorithmInvalid => {
                "Missing an encryption algorithm, or the provided algorithm is not \"AES256\"."
            }
            Self::KeyFormatInvalid => {
                "Missing an encryption key, or it is not Base64 encoded, or it does not meet the required length of the encryption algorithm."
            }
            Self::KeySha256Invalid => {
                "Missing a SHA256 hash of the encryption key, or it is not Base64 encoded, or it does not match the encryption key."
            }
            Self::KeyIncorrect => "The provided encryption key is incorrect.",
            Self::ResourceIsEncrypted => {
                "The resource is encrypted with a customer-supplied encryption key, but the request did not provide one."
            }
            Self::ResourceNotEncrypted => {
                "The resource is not encrypted with a customer-supplied encryption key, but the request provided one."
            }
        }
    }

    pub(crate) fn from_reason(reason: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|error| error.reason() == reason)
    }
}

impl std::fmt::Display for CsekError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

/// A verified key reduced to its SHA-256 fingerprint; the raw key is never retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CustomerKey {
    sha256: [u8; 32],
}

impl CustomerKey {
    pub(crate) fn from_sha256(sha256: [u8; 32]) -> Self {
        Self { sha256 }
    }

    pub(crate) fn sha256(self) -> [u8; 32] {
        self.sha256
    }

    /// Validate the algorithm, the 256-bit key and its declared fingerprint.
    pub(crate) fn parse(
        algorithm: Option<&[u8]>,
        key: Option<&[u8]>,
        sha256: Option<&[u8]>,
    ) -> Result<Option<Self>, CsekError> {
        if algorithm.is_none() && key.is_none() && sha256.is_none() {
            return Ok(None);
        }
        if algorithm != Some(ALGORITHM.as_bytes()) {
            return Err(CsekError::AlgorithmInvalid);
        }
        let key = key
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| crate::json::decode_hash::<32>(value).ok())
            .ok_or(CsekError::KeyFormatInvalid)?;
        let declared = sha256
            .and_then(|value| std::str::from_utf8(value).ok())
            .and_then(|value| crate::json::decode_hash::<32>(value).ok())
            .ok_or(CsekError::KeySha256Invalid)?;
        let digest = ring::digest::digest(&ring::digest::SHA256, &key);
        if digest.as_ref() != declared {
            return Err(CsekError::KeySha256Invalid);
        }
        Ok(Some(Self { sha256: declared }))
    }

    /// Read one key from `prefix` + `algorithm` / `key` / `key-sha256`; repeated headers are invalid.
    pub(crate) fn from_headers(
        headers: &HeaderMap,
        prefix: &str,
    ) -> Result<Option<Self>, CsekError> {
        let single = |suffix: &str, error: CsekError| {
            let name = format!("{prefix}{suffix}");
            let mut values = headers.get_all(name.as_str()).iter();
            let first = values.next();
            if values.next().is_some() {
                return Err(error);
            }
            Ok(first.map(|value| value.as_bytes().trim_ascii()))
        };
        Self::parse(
            single("algorithm", CsekError::AlgorithmInvalid)?,
            single("key", CsekError::KeyFormatInvalid)?,
            single("key-sha256", CsekError::KeySha256Invalid)?,
        )
    }
}

/// Reading object data requires exactly the key the object was written with.
pub(crate) fn check_data(
    stored: Option<[u8; 32]>,
    supplied: Option<CustomerKey>,
) -> Result<(), CsekError> {
    match (stored, supplied) {
        (Some(_), None) => Err(CsekError::ResourceIsEncrypted),
        (Some(stored), Some(key)) if stored != key.sha256 => Err(CsekError::KeyIncorrect),
        (None, Some(_)) => Err(CsekError::ResourceNotEncrypted),
        _ => Ok(()),
    }
}

/// Metadata is readable without a key; a supplied key must still be the right one.
/// Returns whether hashes of the stored bytes may be revealed.
pub(crate) fn check_metadata(
    stored: Option<[u8; 32]>,
    supplied: Option<CustomerKey>,
) -> Result<bool, CsekError> {
    if stored.is_some() && supplied.is_none() {
        return Ok(false);
    }
    check_data(stored, supplied).map(|()| true)
}

/// Whether `name` is one of the three key headers under `prefix`. Other names sharing the
/// prefix (for example `x-goog-encryption-kms-key-name`) are not customer-supplied keys.
pub(crate) fn is_key_header(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|suffix| matches!(suffix, "algorithm" | "key" | "key-sha256"))
}

/// Keys apply only to object data routes; other routes reject them instead of ignoring them.
pub(crate) fn has_headers(headers: &HeaderMap, prefix: &str) -> Option<String> {
    headers
        .keys()
        .find(|name| is_key_header(name.as_str(), prefix))
        .map(|name| name.as_str().to_owned())
}

/// Request headers carrying `key` under `prefix`, for wire tests.
#[cfg(test)]
pub(crate) fn key_headers(prefix: &str, key: &[u8; 32]) -> HeaderMap {
    let digest = ring::digest::digest(&ring::digest::SHA256, key);
    let mut headers = HeaderMap::new();
    for (suffix, value) in [
        ("algorithm", ALGORITHM.to_owned()),
        ("key", crate::json::base64(key)),
        ("key-sha256", crate::json::base64(digest.as_ref())),
    ] {
        headers.insert(
            hyper::header::HeaderName::from_bytes(format!("{prefix}{suffix}").as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    headers
}

/// Fingerprint of a test key, as stored on encrypted objects.
#[cfg(test)]
pub(crate) fn key_sha256(key: &[u8; 32]) -> [u8; 32] {
    ring::digest::digest(&ring::digest::SHA256, key)
        .as_ref()
        .try_into()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_key_and_keeps_only_its_fingerprint() {
        let headers = key_headers(DESTINATION_PREFIX, &[7; 32]);
        let key = CustomerKey::from_headers(&headers, DESTINATION_PREFIX)
            .unwrap()
            .unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, &[7; 32]);
        assert_eq!(key.sha256().as_slice(), digest.as_ref());
        assert_eq!(
            CustomerKey::from_headers(&headers, SOURCE_PREFIX).unwrap(),
            None
        );
        assert_eq!(
            CustomerKey::from_headers(&HeaderMap::new(), DESTINATION_PREFIX).unwrap(),
            None
        );
    }

    #[test]
    fn rejects_invalid_algorithm_key_and_fingerprint() {
        let valid = key_headers(DESTINATION_PREFIX, &[7; 32]);
        let case = |name: &str, value: Option<&str>| {
            let mut headers = valid.clone();
            match value {
                Some(value) => {
                    headers.insert(
                        hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                        value.parse().unwrap(),
                    );
                }
                None => {
                    headers.remove(name);
                }
            }
            CustomerKey::from_headers(&headers, DESTINATION_PREFIX).unwrap_err()
        };
        assert_eq!(
            case("x-goog-encryption-algorithm", Some("AES128")),
            CsekError::AlgorithmInvalid
        );
        assert_eq!(
            case("x-goog-encryption-algorithm", None),
            CsekError::AlgorithmInvalid
        );
        assert_eq!(
            case("x-goog-encryption-key", None),
            CsekError::KeyFormatInvalid
        );
        // 31 bytes: valid base64, wrong AES-256 key length.
        assert_eq!(
            case(
                "x-goog-encryption-key",
                Some(&crate::json::base64(&[7; 31]))
            ),
            CsekError::KeyFormatInvalid
        );
        assert_eq!(
            case("x-goog-encryption-key", Some("not base64!")),
            CsekError::KeyFormatInvalid
        );
        assert_eq!(
            case("x-goog-encryption-key-sha256", None),
            CsekError::KeySha256Invalid
        );
        let other = ring::digest::digest(&ring::digest::SHA256, &[8; 32]);
        assert_eq!(
            case(
                "x-goog-encryption-key-sha256",
                Some(&crate::json::base64(other.as_ref()))
            ),
            CsekError::KeySha256Invalid
        );
        let mut duplicate = valid.clone();
        duplicate.append("x-goog-encryption-key", "AAAA".parse().unwrap());
        assert_eq!(
            CustomerKey::from_headers(&duplicate, DESTINATION_PREFIX).unwrap_err(),
            CsekError::KeyFormatInvalid
        );
    }

    #[test]
    fn data_reads_need_the_matching_key_and_metadata_reads_hide_hashes() {
        let key = CustomerKey::from_sha256([1; 32]);
        let other = CustomerKey::from_sha256([2; 32]);
        assert_eq!(check_data(None, None), Ok(()));
        assert_eq!(check_data(Some([1; 32]), Some(key)), Ok(()));
        assert_eq!(
            check_data(Some([1; 32]), None),
            Err(CsekError::ResourceIsEncrypted)
        );
        assert_eq!(
            check_data(Some([1; 32]), Some(other)),
            Err(CsekError::KeyIncorrect)
        );
        assert_eq!(
            check_data(None, Some(key)),
            Err(CsekError::ResourceNotEncrypted)
        );
        assert_eq!(check_metadata(Some([1; 32]), None), Ok(false));
        assert_eq!(check_metadata(Some([1; 32]), Some(key)), Ok(true));
        assert_eq!(check_metadata(None, None), Ok(true));
        assert_eq!(
            check_metadata(Some([1; 32]), Some(other)),
            Err(CsekError::KeyIncorrect)
        );
    }

    #[test]
    fn error_reasons_round_trip_to_xml_codes() {
        for error in CsekError::ALL {
            assert_eq!(CsekError::from_reason(error.reason()), Some(error));
            let mut code = error.reason().to_owned();
            code[..1].make_ascii_uppercase();
            assert_eq!(error.xml_code(), code);
        }
    }
}
