use super::*;
use crate::storage::sync_state::conflicts::{
    self, Action, Confirmation, ResolveRequest, ResolveTarget,
};

pub(super) async fn list(
    request: Request<RequestBody>,
    root: &RootLease,
    session: &str,
    authority: ControlAuthority,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let mut query = json::parse_query(request.uri().query().unwrap_or_default())?;
    let limit = query
        .remove("limit")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| JsonError::invalid_argument("limit must be 1 through 256"))?
        .unwrap_or(256);
    if !(1..=256).contains(&limit) {
        return Err(JsonError::invalid_argument("limit must be 1 through 256"));
    }
    let mut revision = query.remove("revision");
    if revision
        .as_ref()
        .is_some_and(|value| !super::revision(value))
    {
        return Err(JsonError::invalid_argument(
            "revision must be a conflict listing revision",
        ));
    }
    let ids = root.status();
    let prefix = format!("{}{}{session}", ids.catalog_id, ids.root_id);
    let after = match query.remove("cursor") {
        None => 0,
        Some(cursor)
            if cursor.len() == 144 && cursor.starts_with(&prefix) && hex(&cursor[96..], 48) =>
        {
            if revision
                .as_ref()
                .is_some_and(|value| value != &cursor[96..128])
            {
                return Err(stale());
            }
            revision = Some(cursor[96..128].into());
            i64::from_str_radix(&cursor[128..], 16).map_err(|_| stale())?
        }
        Some(_) => return Err(stale()),
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
    let page = root
        .conflict_page(revision, after, limit, authority)
        .await
        .map_err(error)?;
    let next = page
        .next_after
        .map(|after| format!("{prefix}{}{after:016x}", page.revision));
    bounded(
        &json!({"catalog_id":ids.catalog_id.to_string(),"root_id":ids.root_id.to_string(),"revision":page.revision,"items":page.items,"quota":page.usage,"next_cursor":next}),
    )
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
    if !revision(&request.revision) {
        return Err(JsonError::invalid_argument(
            "Use the exact inspected target revision",
        ));
    }
    let target = match (
        request.conflict_id,
        request.recovery_id,
        request.expected_generation,
        request.target_identity,
        request.confirmation,
    ) {
        (Some(id), None, Some(generation), None, None) if revision(&id) && generation >= 0 => {
            ResolveTarget::Conflict {
                conflict_id: id,
                revision: request.revision,
                expected_generation: Some(generation),
            }
        }
        (None, Some(id), None, Some(identity), Some(confirmation))
            if revision(&id) && revision(&identity) && request.action == Action::Discard =>
        {
            ResolveTarget::Recovery {
                recovery_id: id,
                revision: request.revision,
                target_identity: identity,
                confirmation: Some(confirmation),
            }
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
    registration
        .sender
        .try_send(Command::Resolve {
            request,
            reply,
            identity,
        })
        .map_err(|_| busy())?;
    let outcome = observe(ctx, receive).await?.map_err(error)?;
    let mut response = bounded(&json!(outcome))?;
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
}

pub(super) fn error(error: conflicts::Error) -> JsonError {
    match error {
        conflicts::Error::Root(error) => root_error(&error),
        conflicts::Error::Admin(crate::storage::admin::AdminError::Forbidden) => {
            crate::auth::AuthFailure::Forbidden.into()
        }
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
        conflicts::Error::ConfirmationRequired => JsonError::new(
            StatusCode::BAD_REQUEST,
            "recovery_confirmation_required",
            "An explicit exact-target backup or disposal confirmation is required",
        ),
        conflicts::Error::InvalidResolution => JsonError::new(
            StatusCode::BAD_REQUEST,
            "invalid_recovery_resolution",
            "Use exactly one inspected target and its supported action",
        ),
        conflicts::Error::Admin(
            crate::storage::admin::AdminError::Precondition
            | crate::storage::admin::AdminError::NotFound
            | crate::storage::admin::AdminError::Hold,
        ) => stale(),
        conflicts::Error::Capacity => JsonError::new(
            StatusCode::CONFLICT,
            "conflict_capacity",
            "Resolve retained conflicts before retrying; no source body was discarded",
        ),
        conflicts::Error::Admin(crate::storage::admin::AdminError::DiskFull) => JsonError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "disk_full",
            "Free disk space without deleting pending synchronization data, then retry the same operation ID",
        ),
        conflicts::Error::Admin(_) => JsonError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "io_failure",
            "Preserve retained bytes, restore storage access and retry the same operation ID",
        ),
    }
}
