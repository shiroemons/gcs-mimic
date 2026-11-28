//! AWS's specific HMAC chunk chain, linked by the Google XML header contract.

use super::*;
use ring::hmac;

pub(crate) const STREAMING_PAYLOAD: &str = "STREAMING-AWS4-HMAC-SHA256-PAYLOAD";

/// Whether the body arrives in aws-chunked framing with a signature per chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PayloadFraming {
    Plain,
    AwsChunked,
}

/// Whether the request declares the aws-chunked streaming payload.
pub(crate) fn is_streaming_payload(headers: &HeaderMap) -> bool {
    headers
        .get("x-amz-content-sha256")
        .is_some_and(|value| value == STREAMING_PAYLOAD)
}

pub(super) fn validate(
    method: &Method,
    headers: &HeaderMap,
    algorithm: v4::Algorithm,
    signing: v4::Signing,
) -> Result<PayloadFraming, Failure> {
    let header = matches!(signing, v4::Signing::Header);
    let encoding = headers
        .get("content-encoding")
        .map(|value| value.to_str().map_err(|_| Failure::Malformed))
        .transpose()?;
    let framed = is_streaming_payload(headers);
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
        return Ok(PayloadFraming::Plain);
    }
    let length = sole(headers, "x-amz-decoded-content-length")
        .text(TextOrder::ValueFirst)
        .map_err(|_| Failure::Malformed)?
        .ok_or(Failure::Malformed)?;
    if !header
        || *method != Method::PUT
        || algorithm != v4::Algorithm::AwsHmac
        || headers
            .keys()
            .any(|name| name.as_str().starts_with("x-goog-"))
        || sole(headers, "content-encoding").value().is_err()
        || !matches!(sole(headers, "x-amz-content-sha256").value(), Ok(Some(_)))
        || crate::decimal::Grammar::Digits
            .parse::<u64>(length)
            .is_none()
        || (aws_encoding && !framed)
        || (!transfer && !aws_encoding)
    {
        return Err(Failure::Malformed);
    }
    Ok(if framed {
        PayloadFraming::AwsChunked
    } else {
        PayloadFraming::Plain
    })
}

pub(crate) struct Streaming {
    key: hmac::Key,
    date: String,
    scope: String,
    previous: Vec<u8>,
}

impl Streaming {
    /// The chunk chain under the seed's signing key, starting from the seed signature.
    fn new(key: hmac::Key, scope: &verify::V4Scope, seed: &[u8]) -> Self {
        Self {
            key,
            date: scope.date.clone(),
            scope: scope.scope.clone(),
            previous: seed.to_vec(),
        }
    }

    pub(crate) fn verify_chunk(&mut self, hash: &[u8], signature: &[u8]) -> Result<(), Failure> {
        let message = format!(
            "AWS4-HMAC-SHA256-PAYLOAD\n{}\n{}\n{}\n{}\n{}",
            self.date,
            self.scope,
            encode_lower(&self.previous),
            encode_lower(digest::digest(&digest::SHA256, b"").as_ref()),
            encode_lower(hash)
        );
        hmac::verify(&self.key, message.as_bytes(), signature).map_err(|_| Failure::Mismatch)?;
        self.previous.clear();
        self.previous.extend_from_slice(signature);
        Ok(())
    }
}

impl Auth {
    pub(crate) async fn authenticate_streaming<'a>(
        &'a self,
        keys: &impl HmacKeys,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        clock: &dyn Clock,
    ) -> Result<(Option<Caller<'a>>, Streaming), VerificationError> {
        let prepared = v4::prepare(method, uri, headers, clock.now())?;
        // Only an AWS HMAC signature chains chunk signatures.
        let verify::Scheme::HmacV4(scope) = &prepared.scheme else {
            return Err(Failure::Malformed.into());
        };
        if scope.family != v4::Family::Aws || !is_streaming_payload(headers) {
            return Err(Failure::Malformed.into());
        }
        // Only a signed aws-chunked upload asks for a key while signatures are ignored.
        let signers = self
            .signers()
            .ok_or(Failure::UnknownSigner(KeyLookup::Ignored))?;
        let (caller, secret) = verify::hmac_secret(keys, &signers, &prepared.signer).await?;
        let state = Streaming::new(scope.signing_key(&secret), scope, &prepared.signature);
        // The seed signs the canonical request; the chunk chain starts from the seed signature
        // alone, so a lenient host form does not change how chunks verify.
        verify::verify_host_forms(
            self.host_port.for_signature(&prepared),
            &prepared,
            |message| hmac::verify(&state.key, message.as_bytes(), &prepared.signature).is_ok(),
        )?;
        Ok((caller, state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    fn published_seed(host: &str) -> verify::Prepared {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("host", host),
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
        v4::prepare(
            &Method::PUT,
            &"/examplebucket/chunkObject.txt".parse().unwrap(),
            &headers,
            parse_timestamp("20130524T000000Z").unwrap(),
        )
        .unwrap()
    }

    /// The chunk chain the published demonstration credentials start from the seed.
    fn published_chain(prepared: &verify::Prepared) -> Streaming {
        let verify::Scheme::HmacV4(scope) = &prepared.scheme else {
            panic!("the published seed is a V4 HMAC signature");
        };
        // Published demonstration credentials, never an emulator-created secret.
        let key = scope.signing_key("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
        Streaming::new(key, scope, &prepared.signature)
    }

    #[test]
    fn a_lenient_seed_signature_may_omit_the_request_port_and_chunks_still_chain() {
        // The published seed signs Host without a port; this request adds one.
        let prepared = published_seed("s3.amazonaws.com:4443");
        let mut state = published_chain(&prepared);
        let seed = |host_port| {
            verify::verify_host_forms(host_port, &prepared, |message| {
                hmac::verify(&state.key, message.as_bytes(), &prepared.signature).is_ok()
            })
        };
        assert_eq!(
            seed(SignedHostPort::Exact),
            Err(Failure::MismatchWithHostPort)
        );
        assert_eq!(seed(SignedHostPort::Lenient), Ok(()));
        let hash = digest::digest(&digest::SHA256, &vec![b'a'; 65536]);
        state
            .verify_chunk(
                hash.as_ref(),
                &unhex("ad80c730a21e5b8d04586a2213dd63b9a0e99e0e2307b0ade35a65485a288648").unwrap(),
            )
            .unwrap();
    }

    #[tokio::test]
    async fn published_aws_streaming_seed_and_three_chunk_signatures_match_exactly() {
        let prepared = published_seed("s3.amazonaws.com");
        let mut state = published_chain(&prepared);
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
                .unwrap();
        }
        assert_matches!(
            state.verify_chunk(digest::digest(&digest::SHA256, b"").as_ref(), &[0; 32]),
            Err(Failure::Mismatch)
        );
    }
}
