//! AWS's specific HMAC chunk chain, linked by the Google XML header contract.

use super::*;
use crate::storage::Catalog;
use ring::hmac;

pub(crate) const STREAMING_PAYLOAD: &str = "STREAMING-AWS4-HMAC-SHA256-PAYLOAD";

pub(super) fn validate(
    method: &Method,
    headers: &HeaderMap,
    algorithm: verify::Algorithm,
    signing: v4::Signing,
) -> Result<bool, Failure> {
    let header = matches!(signing, v4::Signing::Header);
    let encoding = headers
        .get("content-encoding")
        .map(|value| value.to_str().map_err(|_| Failure::Malformed))
        .transpose()?;
    let framed = headers
        .get("x-amz-content-sha256")
        .is_some_and(|value| value == STREAMING_PAYLOAD);
    let aws_encoding = encoding.is_some_and(|value| {
        value
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case("aws-chunked"))
    });
    let transfer = header && *method == Method::PUT && headers.contains_key("transfer-encoding");
    if !framed
        && !aws_encoding
        && !transfer
        && !headers.contains_key("x-amz-decoded-content-length")
    {
        return Ok(false);
    }
    let length = headers
        .get("x-amz-decoded-content-length")
        .and_then(|value| value.to_str().ok())
        .ok_or(Failure::Malformed)?;
    if !header
        || *method != Method::PUT
        || algorithm != verify::Algorithm::AwsHmac
        || headers
            .keys()
            .any(|name| name.as_str().starts_with("x-goog-"))
        || headers
            .get_all("x-amz-decoded-content-length")
            .iter()
            .count()
            != 1
        || headers.get_all("content-encoding").iter().count() > 1
        || headers.get_all("x-amz-content-sha256").iter().count() != 1
        || length.is_empty()
        || !length.bytes().all(|byte| byte.is_ascii_digit())
        || length.parse::<u64>().is_err()
        || (aws_encoding && !framed)
        || (!transfer && !aws_encoding)
    {
        return Err(Failure::Malformed);
    }
    Ok(framed)
}

pub(crate) struct Streaming {
    key: hmac::Key,
    date: String,
    scope: String,
    previous: Vec<u8>,
}

impl Streaming {
    fn new(secret: &str, prepared: &verify::Prepared) -> Self {
        let mut key = hmac::Key::new(hmac::HMAC_SHA256, format!("AWS4{secret}").as_bytes());
        for part in prepared.scope.split('/') {
            key = hmac::Key::new(
                hmac::HMAC_SHA256,
                hmac::sign(&key, part.as_bytes()).as_ref(),
            );
        }
        Self {
            key,
            date: prepared
                .message
                .split('\n')
                .nth(1)
                .unwrap_or_default()
                .into(),
            scope: prepared.scope.clone(),
            previous: prepared.signature.clone(),
        }
    }

    pub(crate) async fn verify_chunk(
        &mut self,
        hash: &[u8],
        signature: &[u8],
    ) -> Result<(), VerificationError> {
        let message = format!(
            "AWS4-HMAC-SHA256-PAYLOAD\n{}\n{}\n{}\n{}\n{}",
            self.date,
            self.scope,
            encode_lower(&self.previous),
            encode_lower(digest::digest(&digest::SHA256, b"").as_ref()),
            encode_lower(hash)
        );
        let key = self.key.clone();
        let signature = signature.to_vec();
        let previous = signature.clone();
        if !verify::crypto_job(move || hmac::verify(&key, message.as_bytes(), &signature).is_ok())
            .await?
        {
            return Err(Failure::Mismatch.into());
        }
        self.previous = previous;
        Ok(())
    }
}

impl Auth {
    pub(crate) async fn authenticate_streaming<'a>(
        &'a self,
        catalog: &Catalog,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        clock: &dyn Clock,
    ) -> Result<(Caller<'a>, Streaming), VerificationError> {
        let prepared = v4::prepare(method, uri, headers, clock.now())?;
        if prepared.algorithm != verify::Algorithm::AwsHmac
            || headers
                .get("x-amz-content-sha256")
                .is_none_or(|value| value != STREAMING_PAYLOAD)
        {
            return Err(Failure::Malformed.into());
        }
        let (caller, key) = self.verification_key(catalog, &prepared).await?;
        let verify::Key::Hmac(secret) = key else {
            return Err(Failure::Malformed.into());
        };
        let state = verify::crypto_job(move || {
            let state = Streaming::new(&secret, &prepared);
            hmac::verify(&state.key, prepared.message.as_bytes(), &prepared.signature)
                .map(|()| state)
                .map_err(|_| Failure::Mismatch)
        })
        .await??;
        Ok((caller, state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn published_aws_streaming_seed_and_three_chunk_signatures_match_exactly() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", "s3.amazonaws.com"),
            ("content-encoding", "aws-chunked"),
            ("content-length", "66824"),
            ("x-amz-date", "20130524T000000Z"),
            ("x-amz-decoded-content-length", "66560"),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
            ("x-amz-content-sha256", STREAMING_PAYLOAD),
            (
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,SignedHeaders=content-encoding;content-length;host;x-amz-content-sha256;x-amz-date;x-amz-decoded-content-length;x-amz-storage-class,Signature=4f232c4386841ef735655705268965c44a0e4690baa4adea153f7db9fa80a0a9",
            ),
        ] {
            headers.insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        let prepared = v4::prepare(
            &Method::PUT,
            &"/examplebucket/chunkObject.txt".parse().unwrap(),
            &headers,
            parse_timestamp("20130524T000000Z").unwrap(),
        )
        .unwrap();
        // Published demonstration credentials, never an emulator-created secret.
        let mut state = Streaming::new("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY", &prepared);
        hmac::verify(&state.key, prepared.message.as_bytes(), &prepared.signature).unwrap();
        for (size, signature) in [
            (
                65536,
                "ad80c730a21e5b8d04586a2213dd63b9a0e99e0e2307b0ade35a65485a288648",
            ),
            (
                1024,
                "0055627c9e194cb4542bae2aa5492e3c1575bbb81b612b7d234b86a503ef5497",
            ),
            (
                0,
                "b6c6ea8a5354eaf15b3cb7646744f4275b71ea724fed81ceb9323e279d449df9",
            ),
        ] {
            let hash = digest::digest(&digest::SHA256, &vec![b'a'; size]);
            state
                .verify_chunk(hash.as_ref(), &unhex(signature).unwrap())
                .await
                .unwrap();
        }
        assert!(matches!(
            state
                .verify_chunk(digest::digest(&digest::SHA256, b"").as_ref(), &[0; 32])
                .await,
            Err(VerificationError::Signature(Failure::Mismatch))
        ));
    }
}
