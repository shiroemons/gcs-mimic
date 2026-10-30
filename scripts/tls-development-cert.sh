#!/usr/bin/env bash
# Regenerates the public development CA, leaf certificate and leaf key that the binary embeds
# from src/tls. Review the output, then copy development-ca.pem, development-cert.pem and
# development-key.pem into src/tls. The CA private key is deleted and never committed.
set -euo pipefail

if [ "$#" != 1 ]; then
  echo 'Usage: bash scripts/tls-development-cert.sh EMPTY_DIRECTORY' >&2
  exit 2
fi
mkdir -p "$1"
if [ -n "$(ls -A "$1")" ]; then
  echo 'Certificate output directory must be empty' >&2
  exit 2
fi
cd "$1"
umask 077
trap 'echo "Certificate generation failed:" >&2; sed -n "1,80p" openssl.log >&2' ERR
# Explicit extension sections keep the result independent of the local openssl.cnf. Python
# 3.13+ (VERIFY_X509_STRICT) and other RFC 5280 strict verifiers require the leaf's Authority
# Key Identifier and the CA's Subject Key Identifier and critical basicConstraints.
printf '%s\n' '[req]' 'distinguished_name=dn' 'x509_extensions=ca' 'prompt=no' \
  '[dn]' 'CN=gcs-mimic public development CA' \
  '[ca]' 'basicConstraints=critical,CA:TRUE' 'keyUsage=critical,keyCertSign,cRLSign' \
  'subjectKeyIdentifier=hash' 'authorityKeyIdentifier=keyid:always' > ca.cnf
printf '%s\n' 'basicConstraints=critical,CA:FALSE' \
  'subjectKeyIdentifier=hash' 'authorityKeyIdentifier=keyid:always' \
  'keyUsage=critical,digitalSignature' 'extendedKeyUsage=serverAuth' \
  'subjectAltName=DNS:localhost,DNS:gcs,IP:127.0.0.1,IP:::1' > server.ext
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 \
  -pkeyopt ec_param_enc:named_curve -out ca.key 2>openssl.log
openssl req -x509 -new -key ca.key -sha256 -days 3650 -config ca.cnf \
  -out development-ca.pem 2>openssl.log
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 \
  -pkeyopt ec_param_enc:named_curve -out development-key.pem 2>openssl.log
openssl req -new -key development-key.pem -sha256 \
  -subj '/CN=gcs-mimic public development server' -out server.csr 2>openssl.log
# A fresh random positive serial avoids reusing an issuer/serial pair from an earlier
# generation, which browsers that cached the old leaf reject.
openssl x509 -req -in server.csr -CA development-ca.pem -CAkey ca.key \
  -set_serial "0x$(openssl rand -hex 16 | sed 's/^./4/')" -days 3649 -sha256 \
  -extfile server.ext -out development-cert.pem 2>openssl.log
openssl verify -x509_strict -purpose sslserver -CAfile development-ca.pem \
  development-cert.pem >>openssl.log 2>&1
chmod 644 development-ca.pem development-cert.pem development-key.pem
rm ca.key ca.cnf server.ext server.csr openssl.log
echo 'Generated the ten-year public development CA and leaf for localhost, gcs and loopback IPs.'
