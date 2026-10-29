//! Dispatch for project-scoped JSON resources: HMAC keys and the service agent.
use super::{
    BUCKET_BODY_LIMIT, State, authorize_project, empty_response, json_response,
    validate_bodyless_post,
};
use crate::auth::{AuthFailure, Caller};
use crate::json::{self, Fields, JsonError, ProjectRequest};
use crate::storage::hmac_keys::HmacKeyListQuery;
use http_body_util::Full;
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
};

const HMAC_KEYS_CREATE: &str = "storage.hmacKeys.create";
const HMAC_KEYS_DELETE: &str = "storage.hmacKeys.delete";
const HMAC_KEYS_GET: &str = "storage.hmacKeys.get";
const HMAC_KEYS_LIST: &str = "storage.hmacKeys.list";
const HMAC_KEYS_UPDATE: &str = "storage.hmacKeys.update";

/// `projects.serviceAccount.get` requires `resourcemanager.projects.get`, which no storage role
/// snapshot carries; any local grant in the project stands in for project visibility.
fn authorize_project_visibility(
    caller: Option<Caller<'_>>,
    project: &str,
) -> Result<(), JsonError> {
    match caller {
        None => Ok(()),
        Some(caller) if caller.principal()?.has_project_grant(project) => Ok(()),
        Some(_) => Err(AuthFailure::Forbidden.into()),
    }
}

pub(super) async fn respond(
    request: Request<Incoming>,
    operation: ProjectRequest,
    fields: Fields,
    caller: Option<Caller<'_>>,
    state: &State,
) -> Result<Response<Full<Bytes>>, JsonError> {
    match &operation {
        ProjectRequest::CreateHmacKey { project, .. } => {
            authorize_project(caller, HMAC_KEYS_CREATE, project)?
        }
        ProjectRequest::GetHmacKey { project, .. } => {
            authorize_project(caller, HMAC_KEYS_GET, project)?
        }
        ProjectRequest::ListHmacKeys(params) => {
            authorize_project(caller, HMAC_KEYS_LIST, &params.project)?
        }
        ProjectRequest::UpdateHmacKey { project, .. } => {
            authorize_project(caller, HMAC_KEYS_UPDATE, project)?
        }
        ProjectRequest::DeleteHmacKey { project, .. } => {
            authorize_project(caller, HMAC_KEYS_DELETE, project)?
        }
        ProjectRequest::GetServiceAccount { project } => {
            authorize_project_visibility(caller, project)?
        }
    }
    if !matches!(
        operation,
        ProjectRequest::UpdateHmacKey { .. } | ProjectRequest::CreateHmacKey { .. }
    ) && (request.headers().contains_key("transfer-encoding")
        || request
            .headers()
            .get("content-length")
            .is_some_and(|value| value != "0"))
    {
        return Err(JsonError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    let base_url = &state.api_base_url;
    let mut value = match operation {
        ProjectRequest::CreateHmacKey {
            project,
            service_account_email,
        } => {
            // The pinned Ruby client repeats serviceAccountEmail as a form body on this POST.
            validate_bodyless_post(
                request,
                &[("serviceAccountEmail".into(), service_account_email.clone())].into(),
            )
            .await?;
            let created = state
                .catalog
                .create_hmac_key(&project, &service_account_email)
                .await?;
            json::hmac_key_resource(&created.record, &created.secret, base_url)?
        }
        ProjectRequest::GetHmacKey { project, access_id } => {
            let record = state.catalog.get_hmac_key(&project, &access_id).await?;
            json::hmac_key_metadata(&record, base_url)?
        }
        ProjectRequest::ListHmacKeys(params) => {
            let page = state
                .catalog
                .list_hmac_keys(HmacKeyListQuery {
                    project: params.project.clone(),
                    service_account_email: params.service_account_email.clone(),
                    show_deleted: params.show_deleted_keys,
                    after: params.start_after.clone(),
                    limit: params.max_results as usize,
                })
                .await?;
            json::hmac_key_list(&page.items, &params, page.has_more, base_url)?
        }
        ProjectRequest::UpdateHmacKey { project, access_id } => {
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let update = json::parse_hmac_key_update(&bytes)?;
            let record = state
                .catalog
                .update_hmac_key_state(&project, &access_id, update.state, update.etag)
                .await?;
            json::hmac_key_metadata(&record, base_url)?
        }
        ProjectRequest::DeleteHmacKey { project, access_id } => {
            state.catalog.delete_hmac_key(&project, &access_id).await?;
            return Ok(empty_response(StatusCode::NO_CONTENT));
        }
        ProjectRequest::GetServiceAccount { project } => json::service_account(&project),
    };
    fields.apply(&mut value);
    Ok(json_response(&value))
}
