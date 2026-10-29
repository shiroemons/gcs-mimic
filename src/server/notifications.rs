use super::*;
use crate::json::notifications::{
    NotificationRequest, notification_list, notification_not_found, notification_resource,
    parse_notification,
};

/// Serve `storage.notifications.*`. Configurations are persisted per bucket and read back;
/// gcs-mimic never publishes Pub/Sub messages for them.
pub(super) async fn respond(
    request: Request<Incoming>,
    operation: NotificationRequest,
    fields: json::Fields,
    caller: Option<Caller<'_>>,
    state: &State,
) -> Result<Response<Full<Bytes>>, JsonError> {
    // Official IAM mapping: insert/delete change bucket configuration, get/list read it.
    let permission = match operation {
        NotificationRequest::Insert { .. } | NotificationRequest::Delete { .. } => {
            auth::BUCKETS_UPDATE
        }
        NotificationRequest::List { .. } | NotificationRequest::Get { .. } => auth::BUCKETS_GET,
    };
    let access = authorize_bucket(state, caller, permission, operation.bucket()).await?;
    json::validate_read_condition_headers(request.headers(), json::EtagConditions::Unsupported)?;
    if !matches!(operation, NotificationRequest::Insert { .. })
        && (request.headers().contains_key("transfer-encoding")
            || request
                .headers()
                .get("content-length")
                .is_some_and(|value| value != "0"))
    {
        return Err(JsonError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    let base = &state.api_base_url;
    let mut value = match operation {
        NotificationRequest::Insert { bucket } => {
            let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
            let config = parse_notification(&bytes)?;
            let record = state
                .catalog
                .insert_notification(&bucket, config, &access)
                .await?;
            notification_resource(&record, &bucket, base)
        }
        NotificationRequest::List { bucket } => {
            let records = state.catalog.list_notifications(&bucket, &access).await?;
            notification_list(&records, &bucket, base)
        }
        NotificationRequest::Get {
            bucket,
            notification,
        } => {
            let record = state
                .catalog
                .get_notification(&bucket, &notification, &access)
                .await?
                .ok_or_else(notification_not_found)?;
            notification_resource(&record, &bucket, base)
        }
        NotificationRequest::Delete {
            bucket,
            notification,
        } => {
            if !state
                .catalog
                .delete_notification(&bucket, &notification, &access)
                .await?
            {
                return Err(notification_not_found());
            }
            return Ok(empty_response(StatusCode::NO_CONTENT));
        }
    };
    fields.apply(&mut value);
    Ok(json_response(&value))
}
