#!/usr/bin/env bash
set -euo pipefail

if [ "$#" != 1 ]; then
  echo 'Usage: bash scripts/tls-fixtures.sh EMPTY_DIRECTORY' >&2
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
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
  -subj '/CN=gcs-mimic throwaway development CA' \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -keyout ca.key -out ca.pem 2>openssl.log
openssl req -new -newkey rsa:2048 -nodes -sha256 \
  -subj '/CN=localhost' -keyout server.key -out server.csr 2>openssl.log
printf '%s\n' 'basicConstraints=critical,CA:FALSE' \
  'keyUsage=critical,digitalSignature,keyEncipherment' \
  'extendedKeyUsage=serverAuth' \
  'subjectAltName=DNS:localhost,DNS:gcs,IP:127.0.0.1,IP:::1' > server.ext
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -set_serial 2 \
  -days 2 -sha256 -extfile server.ext -out server.pem 2>openssl.log
sed 's/subjectAltName=.*/subjectAltName=DNS:wrong.invalid/' server.ext > wrong-host.ext
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -set_serial 4 \
  -days 2 -sha256 -extfile wrong-host.ext -out wrong-host.pem 2>openssl.log
touch index.txt
printf '03\n' > serial
printf '%s\n' '[ca]' 'default_ca=local' '[local]' 'database=index.txt' \
  'serial=serial' 'new_certs_dir=.' 'certificate=ca.pem' 'private_key=ca.key' \
  'default_md=sha256' 'policy=names' '[names]' 'commonName=supplied' > ca.cnf
openssl ca -batch -config ca.cnf -in server.csr -extfile server.ext \
  -startdate 200101000000Z -enddate 200102000000Z -notext -out expired.pem 2>openssl.log
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
  -subj '/CN=untrusted test CA' -keyout other.key -out other.pem 2>openssl.log
# The non-root runtime needs read access to explicitly mounted development keys.
chmod 644 ca.pem server.pem expired.pem other.pem server.key
rm openssl.log
echo 'Generated two-day throwaway certificates for localhost, gcs and loopback IPs.'
