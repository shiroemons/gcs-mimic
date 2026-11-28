//! Policy signatures authenticate the exact submitted base64 field, never a canonical request.

use super::*;

impl Auth {
    pub(crate) async fn authenticate_policy_v2<'a>(
        &'a self,
        keys: &impl HmacKeys,
        signer: &str,
        signature: &str,
        policy: &str,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        let Some(signers) = self.signers() else {
            return Ok(None);
        };
        if policy.len() > 64 * 1024 || signer.len() > 1024 || signer.is_empty() {
            return Err(Failure::Malformed.into());
        }
        self.verify_prepared(
            keys,
            v4::Prepared {
                signer: signer.into(),
                scheme: verify::v2_scheme(signers.keys(), signer),
                message: policy.into(),
                // A policy signs the submitted document, never Host.
                portless: None,
                signature: v2::decode_signature(signature)?,
            },
        )
        .await
    }

    pub(crate) async fn authenticate_policy<'a>(
        &'a self,
        keys: &impl HmacKeys,
        algorithm: &str,
        credential: &str,
        date: &str,
        signature: &str,
        policy: &str,
    ) -> Result<Option<Caller<'a>>, VerificationError> {
        if self.signers().is_none() {
            return Ok(None);
        }
        if policy.len() > 64 * 1024 || credential.len() > 1024 {
            return Err(Failure::Malformed.into());
        }
        let algorithm = match algorithm.parse() {
            Ok(algorithm @ (v4::Algorithm::Rsa | v4::Algorithm::GoogleHmac)) => algorithm,
            // A policy form names only the Google algorithms.
            Ok(v4::Algorithm::AwsHmac) | Err(_) => return Err(Failure::Malformed.into()),
        };
        parse_timestamp(date).map_err(|_| Failure::Malformed)?;
        let credential =
            v4::CredentialScope::parse(credential, date, ["storage", "goog4_request"])?;
        let prepared = v4::Prepared {
            signer: credential.signer.into(),
            scheme: algorithm.scheme(date, credential.scope),
            message: policy.into(),
            portless: None,
            signature: unhex(signature)?,
        };
        self.verify_prepared(keys, prepared).await
    }
}
