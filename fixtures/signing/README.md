# Public test-only RSA keys

These throwaway keys are deliberately committed for offline emulator and SDK tests.
Never use this private key outside isolated tests. The server registry accepts only
the public SubjectPublicKeyInfo PEM; the private key is read only by local signers.

```sh
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out fixtures/signing/test-only-private.pem
openssl pkey -in fixtures/signing/test-only-private.pem -pubout -out fixtures/signing/test-only-public.pem
```
