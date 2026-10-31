# Local Pub/Sub notification delivery

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: EXT-BEH-032, EXT-BEH-033 and the delivery effects of JSON-NOTIFICATIONS-001
through JSON-NOTIFICATIONS-004. The feature ledger owns implementation status.
This is a local integration with a separately supplied Pub/Sub emulator, not an
implementation of Pub/Sub or a connection to Google Cloud.

## Configuration

Set `PUBSUB_EMULATOR_HOST=127.0.0.1:8085` or
`GCS_MIMIC_PUBSUB_EMULATOR_HOST=pubsub:8085` to enable delivery to your local
emulator. `--pubsub-emulator-host HOST:PORT` overrides environment and JSON
configuration; the native environment variable overrides `PUBSUB_EMULATOR_HOST`.
The endpoint accepts an authority or an HTTP origin with an explicit nonzero
port. It rejects credentials, non-root paths, queries, fragments and HTTPS.
Hostnames are resolved for each attempt; every resolved address must be loopback,
RFC1918 IPv4 or IPv6 unique-local. The client connects to the checked numeric
address and does not follow redirects. Public, link-local metadata, unspecified
and multicast addresses are rejected, including IPv4-mapped IPv6 forms.
No credentials, OAuth discovery, metadata-server lookup or Google endpoint
fallback exists. Real Google publication is outside this slice.

JSON notification configurations share the existing CRUD API. They begin selecting
events immediately when inserted, even when no publisher endpoint is configured.
Those events remain in the bounded persistent queue until delivery is enabled.
Operators must provision the topic and subscriptions in their Pub/Sub emulator;
gcs-mimic does not create topics or simulate service-agent permissions.

An optional global rule preserves fake-gcs-server's event settings:

| Native option / JSON key | Legacy flag / environment suffix | Effect |
| --- | --- | --- |
| `--event-pubsub-project-id` / `event_pubsub_project_id` | `-event.pubsub-project-id` / `FAKE_GCS_EVENT_PUBSUB_PROJECT_ID` | Project containing the global topic. |
| `--event-pubsub-topic` / `event_pubsub_topic` | `-event.pubsub-topic` / `FAKE_GCS_EVENT_PUBSUB_TOPIC` | Global topic ID. Requires project and explicit emulator endpoint. |
| `--event-bucket` / `event_bucket` | `-event.bucket` / `FAKE_GCS_EVENT_BUCKET` | Exact bucket filter; empty selects all buckets. |
| `--event-object-prefix` / `event_object_prefix` | `-event.object-prefix` / `FAKE_GCS_EVENT_OBJECT_PREFIX` | Object name prefix; empty selects all names. |
| `--event-list` / `event_list` | `-event.list` / `FAKE_GCS_EVENT_LIST` | Comma-separated `finalize`, `delete`, `metadataUpdate`, `archive`; default `finalize`. The local extension `none` disables the global rule, representing an all-false Go NotifyOn. |

The `GCS_MIMIC_` environment forms use the uppercase JSON keys. Incomplete global
settings, unsupported event names and filters without a topic fail configuration
validation. The global rule and JSON configurations select independently: an
object matching both produces two notifications. The global rule has no
`notificationConfig` attribute, matching the legacy manager. Its payload format
is `JSON_API_V1`. Runtime rules are recreated from configuration at startup;
queued snapshots preserve their original topic and content.

## Mutation and payload contract

All conditions, permissions, checksums, holds and retention checks continue through
the existing shared mutation paths. Matching notifications are inserted in that
mutation's SQLite transaction, including the generation allocator and applicable
ACL changes. A rejection or rollback creates neither object changes nor events.
No request holds a SQL transaction while contacting Pub/Sub.

| Transition | Event snapshot |
| --- | --- |
| Successful publication, including upload, copy, rewrite, compose, restore and multipart completion | `OBJECT_FINALIZE` with the completed generation. |
| Object metadata or ACL mutation | `OBJECT_METADATA_UPDATE` with the resulting metageneration and metadata. |
| Replacing or deleting a live generation while versioning preserves it | `OBJECT_ARCHIVE` with the old generation and its retirement time. |
| Removing a generation, including overwrite without versioning and compose source removal | `OBJECT_DELETE` with pre-deletion metadata and `timeDeleted`. |
| Failed/incomplete upload, failed precondition, reads or bucket/configuration changes | No object event. |

An overwrite includes `overwroteGeneration` on finalize and
`overwrittenByGeneration` on the old generation's archive/delete. Filters compare
the logical, decoded object name, with case-sensitive prefix matching. Empty
JSON `event_types` selects the four supported event types. Custom attributes are
copied, then standard event attributes take precedence. Every JSON-configured
message has `notificationConfig`, `eventType`, `payloadFormat`, `bucketId`,
`objectId`, `objectGeneration` and RFC3339 `eventTime` attributes.
`JSON_API_V1` contains the supported JSON object metadata representation without
listener-dependent self/media links, encoded as Pub/Sub base64 data. `NONE` omits
data. The queue stores this immutable snapshot, so subsequent object edits,
deletion or configuration deletion do not change already selected events.

Startup seed and Go constructor InitialObjects suppress notifications, matching
the pinned source's backend initialization before its event manager exists.
This is a trusted request-scoped NotificationMode carried through the same
publication transaction, including retirement of any previous generation.
Runtime local administration, ordinary APIs and sync ingestion continue to emit
selected events. The shared origin and synchronization journal are preserved;
there is no global publisher pause or queue deletion for initialization.
Importing a historical snapshot also suppresses notifications: importing old
state is not replaying historical events. Instance reset clears queued events without
generating object deletion notifications, and retains the operator's global
rule. The queue ID allocator survives reset to avoid reusing an in-flight ID.

## Bounds, retry and lifecycle

The persistent queue is limited to 4096 destination messages and 16 MiB of UTF-8
topic/message bytes, with at most 128 KiB per encoded publish request. SQLite
accounting triggers are the final atomic guard. Capacity exhaustion rejects the
entire mutation with retryable 503 `backendError`; no event is discarded to make
room. Starting or repairing the configured emulator lets acknowledged messages
free capacity. Deleting a notification configuration stops future selection but
does not discard its pending notifications.
The memory backend's separately configured main-catalog limit can be reached
before this queue cap; the queue cap is not a promise of available RAM capacity.

One supervised publisher handles one request at a time and reads one queue row.
Each attempt has a five-second total deadline, bounded HTTP headers and an 8 KiB
response-body cap. Only HTTP 200 with exactly one nonempty `messageIds` entry
acknowledges the message. Network failures, malformed/missing acknowledgements,
redirects and other statuses retain it. Failed messages retry after 100 ms,
doubling to a 30-second maximum; retry count/deadline survive restart. The
scheduler uses the injected clock and earliest due row; it has no idle polling
timer and holds no transaction or reset lease while waiting for future work.
Due-time selection lets another topic progress while one message backs off.

Delivery is at least once. A successful publish followed by a lost response or a
crash before local acknowledgement can publish the same snapshot again, with a
different Pub/Sub message ID. Consumers should use bucket, object, generation,
metageneration and event type for reconciliation. No global or per-object
delivery-order guarantee is provided.

A network attempt holds the shared catalog activity lease through local
acknowledgement or retry recording. Reset reports busy during that attempt; a
successful reset therefore cannot be followed by a new send of a cleared
snapshot. Backoff releases the lease. Catalog close wakes the worker, stops new
attempts, and waits for an active attempt before releasing directory ownership.
Publisher panics are supervised and restarted; repeated failed passes or a
restart gap make readiness false until a successful pass.

## Boundaries

- The four ordinary-object events are supported. Rapid `OBJECT_INITIALIZE`,
  automatic lifecycle execution, analytics and managed-cloud delivery guarantees
  remain outside this slice.
- Soft-delete retirement currently emits the local delete transition; later
  physical expiry does not emit a second event. Provider-exact soft-delete event
  timing has not been established.
- Cloud propagation delay and automatic removal of repeatedly failing
  configurations are not simulated. Configurations remain until explicitly
  deleted; queued data remains until acknowledged or instance reset.
- Local topics and service-agent permissions are not validated by notification
  CRUD. Missing topics are visible publish failures and retained retries.
- The disk backend uses the existing SQLite WAL/FULL and blob publication
  recovery protocol. Fault tests model its seamed boundaries, not physical
  hardware power failure or Google's regional durability.

## Sources and verification

Checked 2026-10-30:

- [Cloud Storage notifications](https://docs.cloud.google.com/storage/docs/pubsub-notifications)
  defines object events, attributes, payloads and at-least-once/no-order guarantees.
- [Pub/Sub publish v1](https://docs.cloud.google.com/pubsub/docs/reference/rest/v1/projects.topics/publish)
  defines the publish request and acknowledgement envelope.
- [Pub/Sub emulator](https://docs.cloud.google.com/pubsub/docs/emulator)
  describes explicit local emulator configuration and separately provisioned
  topics/subscriptions.
- Notification CRUD retains the pinned Discovery revision `20261013` and schema
  digest from [bucket notification configurations](bucket-notifications.md).
- The migration comparison uses fake-gcs-server `v1.56.1`, commit
  `896ece51e49eed0946089f3089f6343855c0d106`, specifically
  `internal/config/config.go` and `internal/notification/event.go`; its in-memory
  fire-and-forget delivery is not the persistence contract implemented here.

The only Cargo feature addition is Hyper's existing `client` feature.
Cargo.lock adds Hyper's `want` 0.3.2 and `try-lock` 0.2.5 dependencies, both MIT.
Official registry index entries checked 2026-10-30 identify these as the newest
non-yanked stable releases. Their compressed packages are 7311 and 4314 bytes,
respectively; these are package sizes, not measured binary growth. Their locked
checksums are `ec4cdd0dd910afe868b7ef477227d8d538b46b3075031afee8a9f2acb0a2ed0b`
and `e421abadd41a4225275504ea4d6566923418b7f05506fbc9c0fe86ba7396114b`.
The publisher repositories for [want](https://github.com/seanmonstar/want) and
[try-lock](https://github.com/seanmonstar/try-lock) were not archived; their last
pushes were 2026-10-05 and 2026-01-20 at the verification time.

Run offline local contracts with:

```sh
cargo check --locked --offline --tests
cargo test --locked --offline --lib notification
cargo test --locked --offline --test notifications_http
cargo xtask lint
```

The real Pub/Sub emulator check is explicitly opt-in and is not run by default.
Start the separately supplied emulator and set a numeric loopback endpoint:

```sh
GCS_MIMIC_TEST_PUBSUB_EMULATOR_HOST=127.0.0.1:8085 \
  cargo test --locked --offline --test notifications_http \
  opted_in_pubsub_emulator_receives_the_notification -- --ignored --exact
```

That test creates unique local topic/subscription names, starts gcs-mimic with
that endpoint, publishes an object, pulls its notification and removes its local
Pub/Sub resources. It never accepts a public or Google endpoint. Actual
execution results belong in [the evidence record](../evidence/pubsub-notification-delivery.md).
