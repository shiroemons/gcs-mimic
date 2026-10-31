//! Policy signatures authenticate the exact submitted base64 field, never a canonical request.

use super::*;
use crate::storage::Catalog;

impl Auth {
    pub(crate) async fn authenticate_policy_v2<'a>(
        &'a self,
        catalog: &Catalog,
        signer: &str,
        signature: &str,
        policy: &str,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        let Some(registry) = &self.registry else {
            return Ok(None);
        };
        if policy.len() > 64 * 1024 || signer.len() > 1024 || signer.is_empty() {
            return Err(Failure::Malformed.into());
        }
        self.verify_prepared(
            catalog,
            v4::Prepared {
                signer: signer.into(),
                algorithm: if registry.signing_keys.contains_key(signer) {
                    v4::Algorithm::Rsa
                } else {
                    v4::Algorithm::LegacyHmacSha1
                },
                scope: String::new(),
                message: policy.into(),
                signature: v2::decode_signature(signature)?,
            },
        )
        .await
        .map(Some)
    }

    pub(crate) async fn authenticate_policy<'a>(
        &'a self,
        catalog: &Catalog,
        algorithm: &str,
        credential: &str,
        date: &str,
        signature: &str,
        policy: &str,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        if self.registry.is_none() {
            return Ok(None);
        }
        if policy.len() > 64 * 1024 || credential.len() > 1024 {
            return Err(Failure::Malformed.into());
        }
        let algorithm = match algorithm {
            ALGORITHM => v4::Algorithm::Rsa,
            "GOOG4-HMAC-SHA256" => v4::Algorithm::GoogleHmac,
            _ => return Err(Failure::Malformed.into()),
        };
        let parts: Vec<_> = credential.split('/').collect();
        if parts.len() != 5
            || parts[0].is_empty()
            || parts[2].is_empty()
            || parts[3..] != ["storage", "goog4_request"]
        {
            return Err(Failure::Malformed.into());
        }
        parse_timestamp(date).map_err(|_| Failure::Malformed)?;
        if parts[1] != &date[..8] {
            return Err(Failure::Malformed.into());
        }
        let prepared = v4::Prepared {
            signer: parts[0].into(),
            algorithm,
            scope: parts[1..].join("/"),
            message: policy.into(),
            signature: unhex(signature)?,
        };
        self.verify_prepared(catalog, prepared).await.map(Some)
    }
}
