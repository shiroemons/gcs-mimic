use super::RequestBody as Incoming;
use super::*;
use crate::json::notifications::{
    NotificationRequest, notification_list, notification_not_found, notification_resource,
    parse_notification_head, topic_not_found, topic_unverifiable,
};

/// Serve `storage.notifications.*`. Configurations are persisted per bucket and read back;
/// gcs-mimic never publishes Pub/Sub messages for them.
pub(super) async fn respond(
    request: Request<Incoming>,
    operation: NotificationRequest,
    fields: json::Fields,
    caller: Option<Caller<'_>>,
    state: &State,
    base: &str,
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
    let mut value = match operation {
        NotificationRequest::Insert { bucket } => {
            let bytes = json::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
            let pending = parse_notification_head(&bytes)?;
            check_topic(state, &bucket, &access, pending.topic()).await?;
            let config = pending.finish()?;
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

/// With a local Pub/Sub emulator configured, reject a topic it does not have, as Cloud Storage
/// does. A lookup that gets no clear answer fails the request rather than accepting the topic.
/// The bucket is read first so a missing bucket is a 404 whatever the emulator would answer,
/// and no outbound lookup is made for it.
async fn check_topic(
    state: &State,
    bucket: &str,
    access: &BucketAccess,
    topic: &str,
) -> Result<(), JsonError> {
    let record = state
        .catalog
        .get_bucket(bucket, access)
        .await?
        .ok_or(CatalogError::NotFound)?;
    match state.catalog.check_notification_topic(topic).await {
        Ok(()) => Ok(()),
        // The service agent named in the message belongs to the bucket's project.
        Err(crate::notification::TopicCheckError::NotFound) => {
            Err(topic_not_found(topic, &record.project))
        }
        Err(error) => Err(topic_unverifiable(&error)),
    }
}
