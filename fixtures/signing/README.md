# Public test-only RSA keys

These throwaway keys are deliberately committed for offline emulator and SDK tests.
Never use this private key outside isolated tests. The server registry accepts only
the public SubjectPublicKeyInfo PEM; the private key is read only by local signers.

This pair is also the key of the built-in test signer
`test-signer@gcs-mimic.invalid`: `src/auth/test_signer.rs` embeds both files into
every gcs-mimic binary and image. Strict mode accepts signatures made with this
private key only with `test_signer=enabled`, because anyone can sign with it.
The container build copies these
two files for that embedding; do not add another copy of the key elsewhere. See
[the built-in test signer](../../docs/en/signed-urls.md#built-in-test-signer).

```sh
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out fixtures/signing/test-only-private.pem
openssl pkey -in fixtures/signing/test-only-private.pem -pubout -out fixtures/signing/test-only-public.pem
```
