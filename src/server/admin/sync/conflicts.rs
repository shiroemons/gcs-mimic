use super::*;
use crate::storage::sync_state::conflicts::{
    self, Action, Confirmation, Conflict, ConflictPage, ConflictPosition, ConflictTarget,
    RecoveryTarget, ResolveRequest, ResolveTarget, Usage,
};

pub(super) async fn list(
    request: Request<RequestBody>,
    root: &RootLease,
    session: &str,
    authority: ControlAuthority,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let mut query = json::parse_query(request.uri().query().unwrap_or_default())?;
    let limit = parse_limit(query.remove("limit"))?;
    let revision = query
        .remove("revision")
        .map(|value| value.parse::<OpaqueId>())
        .transpose()
        .map_err(|_| JsonError::invalid_argument("revision must be a conflict listing revision"))?;
    let ids = root.status();
    let prefix = cursor_prefix(&ids, session);
    let position = match (query.remove("cursor"), revision) {
        (None, None) => ConflictPosition::Start,
        (None, Some(revision)) => ConflictPosition::Within {
            revision,
            after: Ordinal::START,
        },
        (Some(cursor), revision) => {
            let SyncCursor::Conflicts {
                revision: selected,
                ordinal,
            } = SyncCursor::decode(&cursor, &prefix)?
            else {
                return Err(stale());
            };
            if revision.is_some_and(|value| value != selected) {
                return Err(stale());
            }
            ConflictPosition::Within {
                revision: selected,
                after: ordinal,
            }
        }
    };
    if !query.is_empty() {
        return Err(JsonError::invalid_argument(
            "Unknown conflict query parameter",
        ));
    }
    if !json::read_body(request.into_body(), 1).await?.is_empty() {
        return Err(JsonError::invalid_argument(
            "GET synchronization controls do not accept a body",
        ));
    }
    let ConflictPage {
        revision,
        items,
        usage,
        next_after,
    } = root
        .conflict_page(position, limit, authority)
        .await
        .map_err(error)?;
    render_halving(items, |items, fit| {
        let next_cursor = items
            .last()
            .filter(|_| next_after.is_some() || fit == PageFit::Halved)
            .map(|item| {
                SyncCursor::Conflicts {
                    revision,
                    ordinal: item.ordinal,
                }
                .encode(&prefix)
            });
        super::super::encode(&RootDocument::new(
            &ids,
            ConflictsPage {
                revision,
                items,
                quota: &usage,
                next_cursor,
            },
        ))
    })
}

/// A page of `GET /_internal/sync/conflicts`.
#[derive(Serialize)]
struct ConflictsPage<'a> {
    revision: OpaqueId,
    items: &'a [Conflict],
    quota: &'a Usage,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    conflict_id: Option<String>,
    recovery_id: Option<String>,
    revision: String,
    action: Action,
    expected_generation: Option<i64>,
    target_identity: Option<String>,
    confirmation: Option<Confirmation>,
    operation_id: String,
}

fn parse(bytes: &[u8]) -> Result<ResolveRequest, JsonError> {
    let request: Resolve = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::invalid_argument(
            "Send one exact conflict or recovery target, revision, action and operation_id",
        )
    })?;
    let fields: serde_json::Map<String, Value> = serde_json::from_slice(bytes)
        .map_err(|_| JsonError::invalid_argument("Send one resolution object"))?;
    let mixed = if request.conflict_id.is_some() {
        ["recovery_id", "target_identity", "confirmation"]
            .iter()
            .any(|key| fields.contains_key(*key))
    } else {
        ["conflict_id", "expected_generation"]
            .iter()
            .any(|key| fields.contains_key(*key))
    };
    if mixed {
        return Err(JsonError::invalid_argument(
            "Fields for the other resolution target must be absent, including null fields",
        ));
    }
    let Ok(revision) = request.revision.parse::<OpaqueId>() else {
        return Err(JsonError::invalid_argument(
            "Use the exact inspected target revision",
        ));
    };
    let id = |value: String| value.parse::<OpaqueId>();
    let target = match (
        request.conflict_id.map(id),
        request.recovery_id.map(id),
        request.expected_generation,
        request.target_identity.map(id),
        request.confirmation,
    ) {
        (Some(Ok(id)), None, Some(generation), None, None) if generation >= 0 => {
            ResolveTarget::Conflict(ConflictTarget {
                conflict_id: id,
                revision,
                expected_generation: generation,
            })
        }
        (None, Some(Ok(id)), None, Some(Ok(identity)), Some(confirmation))
            if request.action == Action::Discard =>
        {
            ResolveTarget::Recovery(RecoveryTarget {
                recovery_id: id,
                revision,
                target_identity: identity,
                confirmation,
            })
        }
        (None, Some(_), None, Some(_), None) => {
            return Err(JsonError::new(
                StatusCode::BAD_REQUEST,
                "recovery_confirmation_required",
                "Confirm backup_verified or discard_without_backup for the exact inspected recovery identity",
            ));
        }
        _ => {
            return Err(JsonError::new(
                StatusCode::BAD_REQUEST,
                "invalid_recovery_resolution",
                "Select exactly one conflict or recovery target; recovery accepts only explicit confirmed discard",
            ));
        }
    };
    Ok(ResolveRequest {
        operation_id: request.operation_id,
        action: request.action,
        target,
    })
}

pub(super) async fn resolve(
    request: Request<RequestBody>,
    registration: Registration,
    identity: Option<Identity>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, JsonError> {
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Synchronization mutations do not accept query parameters",
        ));
    }
    let bytes = json::read_body(request.into_body(), 64 * 1024).await?;
    let request = parse(&bytes)?;
    let (reply, receive) = tokio::sync::oneshot::channel();
    let command = Command::Resolve {
        request,
        reply,
        identity,
    };
    let outcome = observe(ctx, &registration, command, receive)
        .await?
        .map_err(error)?;
    let mut response = bounded(&super::super::encode(&outcome)?)?;
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
}

pub(in crate::server::admin) fn error(error: conflicts::Error) -> JsonError {
    match error {
        conflicts::Error::Root(error) => root_error(&error),
        conflicts::Error::Admin(error) => admin_failure(&error),
        conflicts::Error::RevisionMismatch => JsonError::new(
            StatusCode::CONFLICT,
            "recovery_revision_mismatch",
            "Fetch the current target revision and inspect its retained body before resolving",
        ),
        conflicts::Error::TargetChanged => JsonError::new(
            StatusCode::CONFLICT,
            "recovery_target_changed",
            "The target changed; preserve retained bytes and inspect its current identity",
        ),
        conflicts::Error::OperationIdReused => JsonError::new(
            StatusCode::CONFLICT,
            "operation_id_reused",
            "Use the original payload for this operation ID or choose a new operation ID",
        ),
        conflicts::Error::InvalidResolution => JsonError::new(
            StatusCode::BAD_REQUEST,
            "invalid_recovery_resolution",
            "Use exactly one inspected target and its supported action",
        ),
        conflicts::Error::Capacity => JsonError::new(
            StatusCode::CONFLICT,
            "conflict_capacity",
            "Resolve retained conflicts before retrying; no source body was discarded",
        ),
    }
}

/// A resolution is idempotent by operation ID, so a failure a retry may clear asks for the same ID.
fn admin_failure(error: &AdminError) -> JsonError {
    match error {
        AdminError::Forbidden => crate::auth::AuthFailure::Forbidden.into(),
        AdminError::Precondition | AdminError::NotFound | AdminError::Hold => stale(),
        AdminError::DiskFull(_) => JsonError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "disk_full",
            "Free disk space without deleting pending synchronization data, then retry the same operation ID",
        ),
        // Contention and a closing catalog clear without a storage repair.
        AdminError::Busy { .. } | AdminError::Closed => JsonError::service_unavailable(
            "Synchronization control is busy or closed; retry the same operation ID after the active operation finishes",
        ),
        AdminError::Corrupt(cause) => corrupt(cause),
        AdminError::SeedProjectMismatch
        | AdminError::SeedMetadataBudget { .. }
        | AdminError::SourceChanged { .. }
        | AdminError::Io(_)
        | AdminError::Invalid(_)
        | AdminError::Checksum
        | AdminError::TooLarge
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => JsonError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "io_failure",
            "Preserve retained bytes, restore storage access and retry the same operation ID",
        ),
    }
}
