# Bucket Pub/Sub notification configurations

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: JSON-NOTIFICATIONS-001 (delete), JSON-NOTIFICATIONS-002 (get),
JSON-NOTIFICATIONS-003 (insert) and JSON-NOTIFICATIONS-004 (list), plus the
AUDIT-021 scope decision for deprecated notification APIs.

gcs-mimic stores, validates and reads back notification configurations.
Configurations also select immutable events into a bounded persistent outbox;
delivery requires an explicitly configured local Pub/Sub emulator endpoint.
See [local notification delivery](pubsub-notification-delivery.md) for the
EXT-BEH-032 event snapshots and EXT-BEH-033 publisher contract, offline capacity,
retry/reset behavior and verification boundaries. The emulator never falls back
to Google Cloud.

## Sources

Checked 2026-10-29:

- Storage v1 Discovery (`compatibility/upstream/json-methods.json`, revision
  `20261013`, SHA-256
  `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`) defines the
  four methods, their `bucket`/`notification`/`userProject` parameters, and the
  `Notification`/`Notifications` schemas in `json-schemas.json`. `topic` and
  `payload_format` are required for insert; `payload_format` defaults to
  `JSON_API_V1`.
- [Notification resource](https://docs.cloud.google.com/storage/docs/json_api/v1/notifications):
  topic form `//pubsub.googleapis.com/projects/PROJECT/topics/TOPIC`, payload
  formats `JSON_API_V1` and `NONE`, empty `event_types` means all events,
  output-only `id`, `etag`, `selfLink` and `kind`.
- [Pub/Sub notifications](https://docs.cloud.google.com/storage/docs/pubsub-notifications):
  event types `OBJECT_FINALIZE`, `OBJECT_METADATA_UPDATE`, `OBJECT_DELETE`,
  `OBJECT_ARCHIVE`; up to 100 configurations per bucket, up to 10 for the same
  event type, up to 10 custom attributes; creating and deleting configurations
  increments the bucket metageneration.
- [IAM permissions for JSON methods](https://docs.cloud.google.com/storage/docs/access-control/iam-json):
  insert and delete require `storage.buckets.update`; get and list require
  `storage.buckets.get`.
- Pinned SDKs: Go `cloud.google.com/go/storage` v1.70.0 sends the full topic
  name; Ruby `google-cloud-storage` 1.62.1 sends the caller's string unchanged
  when it contains `/` (for example `projects/p/topics/t`).

## Supported behavior

| Case | Behavior |
| --- | --- |
| NOTIF-INSERT | `POST b/{bucket}/notificationConfigs` stores one configuration and returns it with `kind`, decimal `id`, `selfLink` under the advertised API base URL, canonical `topic`, `payload_format` and `etag`. Optional `event_types`, `custom_attributes` and `object_name_prefix` are returned only when nonempty. |
| NOTIF-TOPIC | Both `//pubsub.googleapis.com/projects/P/topics/T` and `projects/P/topics/T` are accepted and stored in the full form. Other shapes, empty projects, and topic IDs violating Pub/Sub resource-name rules (3 to 255 characters, leading letter, `[A-Za-z0-9-_.~+%]`, no `goog` prefix) return 400 `invalid`. A missing or null topic returns 400 `required`. |
| NOTIF-FIELDS | `payload_format` defaults to `JSON_API_V1`; other values than `JSON_API_V1`/`NONE` return 400. Event types outside the four documented values, duplicate event types, non-string values, more than 10 custom attributes, attribute keys that are empty, longer than 256 bytes or start with `goog`, values over 1024 bytes and prefixes over 1024 bytes return 400 `invalid`. Malformed JSON returns 400 `parseError`. |
| NOTIF-OUTPUT-ONLY | Request `id`, `etag` and `selfLink` are ignored as server-assigned fields; `kind`, when present, must be `storage#notification`. Other unknown fields follow the existing bucket insert ignored-input rule. |
| NOTIF-LIMITS | Inside the insert transaction, a 101st configuration or an 11th configuration triggering the same event type returns 400 `invalidArgument` without storing anything. A configuration with no `event_types` counts against every event type. |
| NOTIF-GET-LIST-DELETE | `GET .../{id}` returns the stored resource; `GET b/{bucket}/notificationConfigs` returns `kind: storage#notifications` with `items` in ID order (omitted when empty); `DELETE .../{id}` returns 204 with an empty body. Missing, non-canonical (`01`) or non-numeric IDs return 404 `notFound` after bucket authorization. Missing buckets return 404. |
| NOTIF-IDS | IDs are positive decimal strings allocated per bucket and never reused while the bucket exists, including after deleting the highest ID. Configurations are immutable, so the `etag` equals the ID. |
| NOTIF-METAGENERATION | A successful insert or delete increments the bucket metageneration and sets its `updated` time in the same transaction, and appends a bucket-update change-journal entry. Rejected inserts and deletes of absent IDs change nothing. |
| NOTIF-PERSISTENCE | Configurations survive restart. Catalog schema version 28 adds `bucket_notifications` and `bucket_notification_ids`, both referencing `buckets(name)` with `ON DELETE CASCADE`; older catalogs migrate with no configurations. Startup rejects rows whose ID exceeds the bucket allocator, and reads reject stored rows that fail validation. |
| NOTIF-BUCKET-DELETE | Deleting a bucket deletes its configurations and ID allocator in the bucket-delete transaction; a recreated bucket with the same name starts empty with ID 1. |
| NOTIF-QUERY | `fields` is validated against the `Notification`/`Notifications` schemas and applied. `prettyPrint=false` and `alt=json` are accepted; `userProject` and other unsupported common parameters return 400 `badRequest`; unknown or duplicate parameters return 400 `invalidParameter`. `If-Match`/`If-None-Match` return 400 `badRequest`. Non-insert methods require an empty body. |
| NOTIF-METHODS | The collection allows GET and POST; an item allows GET and DELETE. Other methods return 405 with `Allow`. Paths with an empty or slash-containing notification segment are not notification routes. |
| NOTIF-STRICT | Strict profile: insert and delete require `storage.buckets.update`, get and list require `storage.buckets.get`, evaluated against project roles and bucket IAM and rechecked in the catalog transaction. Missing credentials return 401 `required`, invalid credentials 401 `authError`, insufficient grants 403 `forbidden`. |

## Local decisions and known deviations

- Notification CRUD accepts well-formed topics without checking a remote topic
  or service-agent permissions. The optional local emulator publisher retains
  failures and retries; it never creates topics or silently discards events.
- Error reasons and messages for invalid fields, limits and unknown IDs are
  local choices; cloud wire parity has not been observed. Duplicate event types
  are rejected locally; provider behavior is unverified.
- Custom-attribute key/value bounds come from Pub/Sub message-attribute limits;
  whether Cloud Storage enforces them at insert time is unverified.
- `userProject` (requester pays) is not supported, consistent with other routes.
- Advancing `updated` together with the documented metageneration increment
  follows the local bucket-update convention; the provider's `updated` behavior
  is unverified.
- gRPC Storage v2 has no notification RPCs in the pinned protos; XML has no
  notification-configuration API in this slice.

## AUDIT-021: deprecated notification API scope

Object change notification (the legacy watch/channel mechanism) is distinct
from Pub/Sub notification configurations. The pinned Storage v1 Discovery
revision `20261013` contains no `objects.watchAll` or `channels.stop` method,
and the former object-change-notification documentation URL now serves the
Pub/Sub notifications page. gcs-mimic deliberately does **not** recreate watch
or channel endpoints and never returns a false success for them.
`POST /storage/v1/b/{bucket}/o/watch` keeps `watch` as an opaque object name,
whose item route has no POST, so it returns 405 `methodNotAllowed` and creates
nothing; `POST /storage/v1/channels/stop` returns the ordinary unknown-route 404. This decision is not an
inference about Google's shutdown date; it is revisited when a pinned Discovery
revision or official source lists these methods again.

## Verification

See [the evidence record](../evidence/bucket-notifications.md).
Delivery has a [separate evidence record](../evidence/pubsub-notification-delivery.md).
