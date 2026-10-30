//! Structural checks on the embedded development chain. Python 3.13+ (VERIFY_X509_STRICT),
//! recent OpenSSL, Go and webpki reject chains that lack these RFC 5280 properties, and a
//! TLS handshake test with a lenient client would not notice a regeneration that drops them.
//! No X.509 parser is a direct dependency, so a minimal DER reader walks the fields used here.

use super::*;

const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const SUBJECT_KEY_ID: &[u8] = &[0x55, 0x1d, 0x0e];
const AUTHORITY_KEY_ID: &[u8] = &[0x55, 0x1d, 0x23];
const EXTENDED_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
const SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const PRIME256V1: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const DIGITAL_SIGNATURE: u8 = 0x80;
const KEY_CERT_SIGN: u8 = 0x04;

/// Splits one DER TLV with a single-byte tag into (tag, contents, rest).
fn tlv(input: &[u8]) -> (u8, &[u8], &[u8]) {
    let (&tag, input) = input.split_first().expect("DER tag");
    let (&first, mut input) = input.split_first().expect("DER length");
    let len = if first < 0x80 {
        usize::from(first)
    } else {
        let count = usize::from(first & 0x7f);
        assert!((1..=4).contains(&count), "unsupported DER length form");
        let (bytes, rest) = input.split_at(count);
        input = rest;
        bytes
            .iter()
            .fold(0, |len, &byte| (len << 8) | usize::from(byte))
    };
    let (contents, rest) = input.split_at(len);
    (tag, contents, rest)
}

fn children(mut input: &[u8]) -> Vec<(u8, &[u8])> {
    let mut items = Vec::new();
    while !input.is_empty() {
        let (tag, contents, rest) = tlv(input);
        items.push((tag, contents));
        input = rest;
    }
    items
}

struct Extension<'a> {
    critical: bool,
    value: &'a [u8],
}

struct Certificate<'a> {
    issuer: &'a [u8],
    subject: &'a [u8],
    key_algorithm: Vec<(u8, &'a [u8])>,
    extensions: Vec<(&'a [u8], Extension<'a>)>,
}

impl<'a> Certificate<'a> {
    fn parse(der: &'a [u8]) -> Self {
        let (_, certificate, _) = tlv(der);
        let (_, tbs, _) = tlv(certificate);
        let fields = children(tbs);
        assert_eq!(fields[0].0, 0xa0, "X.509 v3 version field");
        let (_, algorithm, _) = tlv(fields[6].1);
        let (_, extensions, _) = tlv(fields
            .iter()
            .find(|(tag, _)| *tag == 0xa3)
            .expect("X.509 v3 extensions")
            .1);
        let extensions = children(extensions)
            .into_iter()
            .map(|(_, extension)| {
                let parts = children(extension);
                let critical = parts.len() == 3 && parts[1] == (0x01, &[0xff][..]);
                (
                    parts[0].1,
                    Extension {
                        critical,
                        value: parts[parts.len() - 1].1,
                    },
                )
            })
            .collect();
        Self {
            issuer: fields[3].1,
            subject: fields[5].1,
            key_algorithm: children(algorithm),
            extensions,
        }
    }

    fn extension(&self, oid: &[u8]) -> &Extension<'a> {
        let mut matches = self.extensions.iter().filter(|(id, _)| *id == oid);
        let Some((_, extension)) = matches.next() else {
            panic!("required extension with OID bytes {oid:02x?} is missing");
        };
        assert!(
            matches.next().is_none(),
            "extension {oid:02x?} appears once"
        );
        extension
    }

    fn key_usage(&self) -> u8 {
        let extension = self.extension(KEY_USAGE);
        assert!(extension.critical, "keyUsage is critical");
        let (tag, bits, _) = tlv(extension.value);
        assert_eq!(tag, 0x03);
        bits[1]
    }

    fn subject_key_id(&self) -> &'a [u8] {
        let (tag, key_id, _) = tlv(self.extension(SUBJECT_KEY_ID).value);
        assert_eq!(tag, 0x04);
        assert!(!key_id.is_empty());
        key_id
    }

    fn authority_key_id(&self) -> &'a [u8] {
        let (_, fields, _) = tlv(self.extension(AUTHORITY_KEY_ID).value);
        let (tag, key_id, _) = tlv(fields);
        assert_eq!(tag, 0x80, "authorityKeyIdentifier carries keyIdentifier");
        key_id
    }

    fn basic_constraints(&self) -> Vec<(u8, &'a [u8])> {
        let extension = self.extension(BASIC_CONSTRAINTS);
        assert!(extension.critical, "basicConstraints is critical");
        let (_, fields, _) = tlv(extension.value);
        children(fields)
    }
}

fn single_certificate(pem: &[u8]) -> CertificateDer<'static> {
    let mut certificates = certificates(pem).unwrap();
    assert_eq!(certificates.len(), 1);
    certificates.remove(0)
}

fn assert_named_p256(certificate: &Certificate<'_>) {
    assert_eq!(
        certificate.key_algorithm,
        vec![(0x06, EC_PUBLIC_KEY), (0x06, PRIME256V1)],
        "P-256 key with a named curve"
    );
}

#[test]
fn development_ca_is_a_strict_root() {
    let der = single_certificate(DEVELOPMENT_CA_PEM.as_bytes());
    let ca = Certificate::parse(&der);
    assert_eq!(ca.issuer, ca.subject, "self-signed root");
    assert_named_p256(&ca);
    assert_eq!(ca.basic_constraints(), vec![(0x01, &[0xff][..])]);
    assert_ne!(ca.key_usage() & KEY_CERT_SIGN, 0);
    assert_eq!(ca.authority_key_id(), ca.subject_key_id());
}

#[test]
fn development_leaf_carries_strict_server_extensions() {
    let ca_der = single_certificate(DEVELOPMENT_CA_PEM.as_bytes());
    let ca = Certificate::parse(&ca_der);
    let der = single_certificate(DEVELOPMENT_CERT_PEM);
    let leaf = Certificate::parse(&der);
    assert_eq!(leaf.issuer, ca.subject);
    assert_named_p256(&leaf);
    assert!(leaf.basic_constraints().is_empty(), "leaf is not a CA");
    assert_eq!(
        leaf.key_usage() & (DIGITAL_SIGNATURE | KEY_CERT_SIGN),
        DIGITAL_SIGNATURE
    );
    assert_eq!(leaf.authority_key_id(), ca.subject_key_id());
    assert_ne!(leaf.subject_key_id(), ca.subject_key_id());

    let (_, purposes, _) = tlv(leaf.extension(EXTENDED_KEY_USAGE).value);
    assert!(children(purposes).contains(&(0x06, SERVER_AUTH)));

    let names = leaf.extension(SUBJECT_ALT_NAME);
    assert!(
        !names.critical,
        "subject is not empty, so SAN stays non-critical"
    );
    let (_, names, _) = tlv(names.value);
    let loopback_v6 = std::net::Ipv6Addr::LOCALHOST.octets();
    assert_eq!(
        children(names),
        vec![
            (0x82, &b"localhost"[..]),
            (0x82, &b"gcs"[..]),
            (0x87, &[127, 0, 0, 1][..]),
            (0x87, &loopback_v6[..]),
        ]
    );
}
