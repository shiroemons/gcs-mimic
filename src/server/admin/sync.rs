//! Live synchronization controls never open a data directory or confer fixture authority.

use crate::{
    auth::Identity,
    json::{self, JsonError},
    server::{RequestBody, RequestCtx, json_response},
    storage::sync_state::{
        authority::ControlAuthority,
        deletions::DeletionSet,
        root::{RootError, RootLease},
    },
    sync::control::{Command, Registration},
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Bytes};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io, sync::Arc};

const RESPONSE_LIMIT: usize = 256 * 1024;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

mod conflicts;
mod findings;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approve {
    deletion_set: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rescan {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wait {
    timeout_ms: Option<u64>,
}

pub(super) async fn respond(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Response<Full<Bytes>> {
    control(request, ctx)
        .await
        .unwrap_or_else(JsonError::into_response)
}

async fn control(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let (registration, root) = ctx
        .state
        .catalog
        .sync_controls()
        .map_err(JsonError::from)?
        .ok_or_else(|| {
            JsonError::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Directory synchronization is not running",
            )
        })?;
    let _admission = registration
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| busy())?;
    let caller =
        ctx.state
            .auth
            .authenticate_at(request.headers(), None, ctx.state.clock.as_ref())?;
    let identity = caller
        .map(|caller| caller.principal().cloned())
        .transpose()?;
    root.authorize_controls(ctx.config.seed_project.clone(), identity.clone())
        .await
        .map_err(|error| root_error(&error))?;
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    tests::authorization_checkpoint().await;
    let path = request.uri().path();
    let method = match path {
        "/_internal/sync" | "/_internal/sync/conflicts" => Method::GET,
        "/_internal/sync/approve"
        | "/_internal/sync/rescan"
        | "/_internal/sync/resolve"
        | "/_internal/sync/wait" => Method::POST,
        _ => {
            return Err(JsonError::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Unknown synchronization control route",
            ));
        }
    };
    if request.method() != method {
        let mut error = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "Use the method documented for this synchronization control",
        );
        error.allow = Some(if method == Method::GET { "GET" } else { "POST" });
        return Err(error);
    }
    match path {
        "/_internal/sync/conflicts" => {
            conflicts::list(
                request,
                &root,
                &registration.session,
                ControlAuthority::new(ctx.config.seed_project.clone(), identity),
            )
            .await
        }
        "/_internal/sync/resolve" => conflicts::resolve(request, registration, identity, ctx).await,
        "/_internal/sync" => {
            let params = Page::parse(request.uri().query())?;
            let bytes = json::read_body(request.into_body(), 1).await?;
            if !bytes.is_empty() {
                return Err(JsonError::invalid_argument(
                    "GET synchronization controls do not accept a body",
                ));
            }
            status(
                root,
                &params,
                ctx.state,
                &registration.session,
                ControlAuthority::new(ctx.config.seed_project.clone(), identity),
            )
            .await
        }
        "/_internal/sync/approve" | "/_internal/sync/rescan" => {
            mutate(request, registration, identity, ctx).await
        }
        "/_internal/sync/wait" => wait(request, registration, identity, ctx).await,
        _ => Err(JsonError::new(
            StatusCode::NOT_FOUND,
            "notFound",
            "Unknown synchronization control route",
        )),
    }
}

async fn wait(
    request: Request<RequestBody>,
    registration: Registration,
    identity: Option<Identity>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, JsonError> {
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Synchronization waits do not accept query parameters",
        ));
    }
    let bytes = json::read_body(request.into_body(), 64 * 1024).await?;
    let value: Wait = serde_json::from_slice(&bytes).map_err(|_| {
        JsonError::invalid_argument("Send an empty JSON object or timeout_ms from 1 through 60000")
    })?;
    let timeout = value.timeout_ms.unwrap_or(10_000);
    if !(1..=60_000).contains(&timeout) {
        return Err(JsonError::invalid_argument(
            "timeout_ms must be 1 through 60000",
        ));
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
    // The bounded handler deadline replaces connection inactivity while the server observes work.
    ctx.activity.send_replace(crate::server::ObjectActivity {
        awaiting_catalog: true,
        deadline: Some(deadline),
    });
    let (reply, receive) = tokio::sync::oneshot::channel();
    registration
        .sender
        .try_send(Command::Wait {
            deadline,
            reply,
            identity,
        })
        .map_err(|_| busy())?;
    let observed = tokio::time::timeout_at(deadline, receive).await;
    ctx.finish_object_request();
    let result = match observed {
        Ok(result) => result
            .map_err(|_| busy())?
            .map_err(|error| control_error(&error))?,
        Err(_) => crate::sync::control::WaitResult::timeout(),
    };
    let mut response = bounded(&json!(result))?;
    if result.timed_out {
        *response.status_mut() = StatusCode::REQUEST_TIMEOUT
    } else if !result.completed {
        *response.status_mut() = StatusCode::CONFLICT
    }
    Ok(response)
}

async fn mutate(
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
    let approve = request.uri().path() == "/_internal/sync/approve";
    let bytes = json::read_body(request.into_body(), 64 * 1024).await?;
    let value = if approve {
        let body: Approve = serde_json::from_slice(&bytes).map_err(|_| {
            JsonError::invalid_argument("Send exactly one string deletion_set revision")
        })?;
        if !revision(&body.deletion_set) {
            return Err(JsonError::invalid_argument(
                "deletion_set must be the exact revision from status",
            ));
        }
        let (reply, receive) = tokio::sync::oneshot::channel();
        registration
            .sender
            .try_send(Command::Approve {
                revision: body.deletion_set,
                reply,
                identity,
            })
            .map_err(|_| busy())?;
        let set = observe(ctx, receive)
            .await?
            .map_err(|error| control_error(&error))?;
        json!({"operation_id":set.revision,"candidate_count":set.candidate_count})
    } else {
        serde_json::from_slice::<Rescan>(&bytes).map_err(|_| {
            JsonError::invalid_argument("Send an empty JSON object to request a rescan")
        })?;
        let mut operation = [0; 16];
        getrandom::fill(&mut operation).map_err(|_| busy())?;
        let (reply, receive) = tokio::sync::oneshot::channel();
        registration
            .sender
            .try_send(Command::Rescan { reply, identity })
            .map_err(|_| busy())?;
        let summary = observe(ctx, receive)
            .await?
            .map_err(|error| control_error(&error))?;
        json!({"operation_id":crate::hashing::encode_lower(&operation),"completed":true,"reconciliation":summary})
    };
    let mut response = bounded(&value)?;
    *response.status_mut() = StatusCode::ACCEPTED;
    Ok(response)
}

async fn observe<T>(
    ctx: RequestCtx<'_>,
    receive: tokio::sync::oneshot::Receiver<T>,
) -> Result<T, JsonError> {
    ctx.activity.send_replace(crate::server::ObjectActivity {
        awaiting_catalog: true,
        deadline: None,
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(ctx.config.object_request_timeout_ms),
        receive,
    )
    .await;
    ctx.finish_object_request();
    result.map_err(|_| busy())?.map_err(|_| busy())
}

#[derive(Default)]
struct Page {
    view: View,
    revision: Option<String>,
    limit: usize,
    after: Option<String>,
}
#[derive(Default, PartialEq, Eq)]
enum View {
    #[default]
    Summary,
    Deletions,
    Findings,
}
impl Page {
    fn parse(query: Option<&str>) -> Result<Self, JsonError> {
        let mut values = json::parse_query(query.unwrap_or_default())?;
        if values.is_empty() {
            return Ok(Self::default());
        }
        let view = match values.remove("view").as_deref() {
            Some("deletions") => View::Deletions,
            Some("findings") => View::Findings,
            _ => {
                return Err(JsonError::invalid_argument(
                    "Use view=deletions or view=findings with the corresponding current revision",
                ));
            }
        };
        let revision = values
            .remove("revision")
            .filter(|value| {
                if view == View::Deletions {
                    revision(value)
                } else {
                    hex(value, 48)
                }
            })
            .ok_or_else(|| {
                JsonError::invalid_argument(
                    "A paged view requires its current revision from status",
                )
            })?;
        let limit = values
            .remove("limit")
            .map(|value| value.parse::<usize>())
            .transpose()
            .map_err(|_| JsonError::invalid_argument("limit must be 1 through 256"))?
            .unwrap_or(256);
        if !(1..=256).contains(&limit) {
            return Err(JsonError::invalid_argument("limit must be 1 through 256"));
        }
        let after = values.remove("cursor");
        if !values.is_empty() {
            return Err(JsonError::invalid_argument(
                "Unknown synchronization query parameter",
            ));
        }
        Ok(Self {
            view,
            revision: Some(revision),
            limit,
            after,
        })
    }
}

async fn status(
    root: Arc<RootLease>,
    page: &Page,
    state: &crate::server::State,
    session: &str,
    authority: ControlAuthority,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let mut after = 0;
    loop {
        let recoveries = root
            .recovery_page(after, 128, authority.clone())
            .await
            .map_err(conflicts::error)?;
        let Some(last) = recoveries.last() else { break };
        after = last.ordinal;
    }
    let observation = root
        .observation_for(authority.clone())
        .await
        .map_err(|error| root_error(&error))?;
    let findings_revision = format!("{session}{:016x}", observation.revision);
    if page.view == View::Findings {
        if page.revision.as_deref() != Some(&findings_revision) {
            return Err(stale());
        }
        return findings_page(&root, page, &findings_revision, &observation, authority).await;
    }
    let set = root
        .deletion_set_for(authority.clone())
        .await
        .map_err(|error| root_error(&error))?;
    let ids = root.status();
    if let Some(revision) = &page.revision {
        let set = set
            .filter(|set| set.revision == *revision)
            .ok_or_else(stale)?;
        return deletion_page(&root, page, set, authority).await;
    }
    let pending = set
        .as_ref()
        .map_or(0, |set| set.candidate_count - set.completed_count);
    let paused = observation
        .paused_reason
        .as_deref()
        .map(|reason| match reason {
            "ConflictCapacity" | "IoFailure" | "RecoveryRequired" => reason,
            _ => "Paused",
        });
    bounded(
        &json!({"catalog_id":ids.catalog_id.to_string(),"root_id":ids.root_id.to_string(),"auth_mode":state.auth.mode().as_str(),
        "state":if let Some(reason)=paused {reason} else if !state.is_ready() {"Unhealthy"} else if observation.writeback.as_ref().is_some_and(|value|value.state!="Ready") {observation.writeback.as_ref().map(|value|value.state).unwrap_or("Observing")} else if observation.scan_stop.is_some() {observation.scan_stop.as_deref().unwrap_or("Observing")} else if !observation.baseline_complete {"Observing"} else if set.as_ref().is_some_and(|set|set.guarded && set.authorization==0) {"DeletionApproval"} else if observation.path_stops+observation.mapping_findings+observation.pending_writeback+observation.unresolved_conflicts>0 {"FindingsPending"} else {"Ready"},
        "session_id":session,"findings_revision":findings_revision,"observation":observation,
        "pending_deletion_revision":set.as_ref().filter(|_|pending>0).map(|set|set.revision.clone()),"pending_deletion_count":pending,
            "deletion_set":set,"writeback_safety":"preserve-pending-work","conflict_resolution_available":true,"stable_wait_available":true}),
    )
}

async fn findings_page(
    root: &RootLease,
    page: &Page,
    revision: &str,
    observation: &crate::storage::sync_state::observation::Summary,
    authority: ControlAuthority,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let ids = root.status();
    let prefix = format!("{}{}{revision}", ids.catalog_id, ids.root_id);
    let after = match &page.after {
        None => (0, 0),
        Some(cursor)
            if cursor.len() == 130 && cursor.starts_with(&prefix) && hex(&cursor[112..], 18) =>
        {
            let kind = i64::from_str_radix(&cursor[112..114], 16).map_err(|_| stale())?;
            let ordinal = i64::from_str_radix(&cursor[114..], 16).map_err(|_| stale())?;
            if kind > 4 {
                return Err(stale());
            }
            (kind, ordinal)
        }
        Some(_) => return Err(stale()),
    };
    let mut items = root
        .observation_page_for(observation.revision, after, page.limit + 1, authority)
        .await
        .map_err(|error| root_error(&error))?;
    let full = items.len() > page.limit;
    items.truncate(page.limit);
    let mut truncated = false;
    loop {
        let next = items
            .last()
            .filter(|_| full || truncated)
            .map(|item| format!("{prefix}{:02x}{:016x}", item.kind, item.ordinal));
        let value = json!({"catalog_id":ids.catalog_id.to_string(),"root_id":ids.root_id.to_string(),"view":"findings","revision":revision,
            "total_count":observation.mapping_findings+observation.pending_writeback+observation.path_stops+observation.unresolved_conflicts+u64::from(observation.paused_reason.is_some())+u64::from(observation.scan_stop.is_some()),"items":items.iter().map(|item|findings::value(item,&prefix,observation.writeback.as_ref())).collect::<Vec<_>>(),"next_cursor":next});
        if value.to_string().len() <= RESPONSE_LIMIT {
            return bounded(&value);
        }
        if items.len() <= 1 {
            return Err(busy());
        }
        items.truncate(items.len() / 2);
        truncated = true;
    }
}

async fn deletion_page(
    root: &RootLease,
    page: &Page,
    set: DeletionSet,
    authority: ControlAuthority,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let ids = root.status();
    let prefix = format!("{}{}{}", ids.catalog_id, ids.root_id, set.revision);
    let after = match &page.after {
        None => 0,
        Some(cursor) if cursor.len() == 112 && cursor.starts_with(&prefix) => {
            i64::from_str_radix(&cursor[96..], 16).map_err(|_| stale())?
        }
        Some(_) => return Err(stale()),
    };
    let mut items = root
        .deletion_page_for(set.revision.clone(), after, page.limit, authority)
        .await
        .map_err(|error| root_error(&error))?;
    loop {
        let last = items.last().map_or(after, |item| item.ordinal);
        let next = ((last as u64) < set.candidate_count).then(|| format!("{prefix}{last:016x}"));
        let value = json!({"catalog_id":ids.catalog_id.to_string(),"root_id":ids.root_id.to_string(),"view":"deletions","revision":set.revision,
            "total_count":set.candidate_count,"scan_revision":set.scan_revision,"journal_fence":set.journal_fence,
            "baseline_count":set.baseline_count,"candidate_count":set.candidate_count,"guard_enabled":set.guard_enabled,
            "min_objects":100,"percent":50,"items":items,"next_cursor":next});
        if value.to_string().len() <= RESPONSE_LIMIT {
            return bounded(&value);
        }
        if items.len() <= 1 {
            return Err(busy());
        }
        items.truncate(items.len() / 2);
    }
}

fn revision(value: &str) -> bool {
    hex(value, 32)
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn bounded(value: &Value) -> Result<Response<Full<Bytes>>, JsonError> {
    if value.to_string().len() > RESPONSE_LIMIT {
        Err(busy())
    } else {
        Ok(json_response(value))
    }
}
fn stale() -> JsonError {
    JsonError::new(
        StatusCode::CONFLICT,
        "conflict",
        "Synchronization revision changed; rescan, fetch status and inspect the new revision before approval",
    )
}
fn busy() -> JsonError {
    JsonError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "backendError",
        "Synchronization control is busy or closed; retry after the active operation finishes",
    )
}
fn root_error(error: &RootError) -> JsonError {
    match error {
        RootError::Admin(crate::storage::admin::AdminError::Forbidden) => {
            crate::auth::AuthFailure::Forbidden.into()
        }
        RootError::Admin(crate::storage::admin::AdminError::Precondition) => stale(),
        RootError::Admin(_) | RootError::InUse => busy(),
        RootError::Unsupported => JsonError::new(
            StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
            "Directory synchronization is unsupported on this platform",
        ),
        _ => JsonError::new(
            StatusCode::CONFLICT,
            "conflict",
            "Synchronization root cannot be verified; preserve the tree and workspace and inspect server diagnostics",
        ),
    }
}
fn control_error(error: &io::Error) -> JsonError {
    let mut cause = error
        .get_ref()
        .map(|error| error as &(dyn std::error::Error + 'static));
    while let Some(error) = cause {
        if let Some(root) = error.downcast_ref::<RootError>() {
            return root_error(root);
        }
        if let Some(crate::storage::admin::AdminError::Forbidden) =
            error.downcast_ref::<crate::storage::admin::AdminError>()
        {
            return crate::auth::AuthFailure::Forbidden.into();
        }
        if matches!(
            error.downcast_ref::<crate::storage::admin::AdminError>(),
            Some(
                crate::storage::admin::AdminError::Precondition
                    | crate::storage::admin::AdminError::Hold
                    | crate::storage::admin::AdminError::NotFound
            )
        ) {
            return stale();
        }
        cause = if let Some(error) = error.downcast_ref::<io::Error>() {
            error
                .get_ref()
                .map(|error| error as &(dyn std::error::Error + 'static))
        } else {
            error.source()
        };
    }
    busy()
}
