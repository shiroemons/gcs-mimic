# Permission denial message verification

<!-- markdownlint-configure-file {"MD013": false} -->

DESIGN-002 remains PARTIAL. This evidence covers local strict-mode behavior; it does not establish Google, SDK, container or platform fidelity, and no real-service request was made.

## Sources

Checked 2026-11-08:

- The [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) reference documents only the 401 `required` example `Login Required` (`locationType` `header`, `location` `Authorization`) and the 403 `forbidden` example `Forbidden`.
- The [IAM permission error messages](https://docs.cloud.google.com/iam/docs/permission-error-messages) page shows the project-level form `EMAIL_ADDRESS does not have storage.buckets.list access to the Google Cloud project. Permission 'storage.buckets.list' denied on resource (or it may not exist).`
- The [Cloud Storage troubleshooting](https://docs.cloud.google.com/storage/docs/troubleshooting) page shows `example@email.com doesn't have storage.objects.get access to the Google Cloud Storage object` and mentions error messages that refer to `Anonymous caller`.
- The [XML status reference](https://docs.cloud.google.com/storage/docs/xml-api/reference-status) lists `AccessDenied` and `AuthenticationRequired` without message text.

No official page shows a complete bucket- or object-level sentence. The bucket noun, the second sentence on bucket and object denials and the complete anonymous sentence are local choices recorded under SPEC-017. A recorded `objects.move` denial in `compatibility/differential/reference/gcs.json` named the resource path in its second sentence; the other local denials omit it.

## Verified behavior

- Strict authorization carries a typed denial (principal, Cloud Storage permission and project, bucket or object resource) through `AuthFailure`, `CatalogError` and `ObjectError` into the JSON and XML errors. Statuses, reasons and XML codes are unchanged.
- JSON 403 `forbidden` and XML 403 `AccessDenied` messages name the registered id: the token holder, the trusted-listener principal or the signer of a signed URL. Anonymous bucket and object denials keep JSON 401 `required` at the `Authorization` header and XML 401 `AuthenticationRequired`, with `Anonymous caller` as the principal.
- Project-level anonymous refusals keep `Login Required`. Refusals that no Cloud Storage permission names (project visibility, administration routes, sync controls) keep `Forbidden`. Public access prevention and retention keep their own reasons.
- Replacing a live object without `storage.objects.delete` names that permission. A denial for a missing bucket or object reads exactly like one for an existing resource; callers holding the permission in some project still receive 404.
- At `--log-level debug` each refused response logs one `authorization_denied` event with the principal or `anonymous`, the permission, the resource kind and the bucket or project. At `info` nothing is logged, and refusals that `testIamPermissions` discards log nothing. Events carry no tokens.

## Executed checks

| Command | Result |
| --- | --- |
| `cargo check --workspace --all-targets --locked` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS |
| `cargo xtask lint` | PASS: 0 new production violations |
| `cargo test --locked --lib -- auth:: json:: xml:: storage:: server:: grpc::` | PASS: 1427 tests |
| `cargo test --locked --test it_xml_auth` | PASS: 283 tests |
| `cargo test --locked --test it_json` | PASS: 135 tests |
| `cargo test --locked --test it_objects` | PASS: 144 tests |

## Limitations

gRPC statuses are built from the same JSON errors, so by construction a registered caller's gRPC `PERMISSION_DENIED` message takes the same form; no gRPC test asserts it. gRPC anonymous refusals stay `UNAUTHENTICATED`: the v1 bucket check goes through the JSON anonymous mapping and names the anonymous caller, while the other gRPC paths keep `Login Required`. gRPC responses log no `authorization_denied` event. The `lib` gRPC tests passed unchanged. The full test suite, SDK suites and container checks were not run for this change.
