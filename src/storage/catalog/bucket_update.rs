use super::*;

const UPDATE_BUCKET: &str = "UPDATE buckets SET labels = ?2, storage_class = ?3,
                     versioning_enabled = ?4, versioning_configured = ?14, default_event_based_hold = ?5,
                     metageneration = ?6, updated = ?7, updated_nanos = 0, cors = ?8, public_access_prevention_enforced = ?9,
                     uniform_bucket_level_access_locked_time = ?10, lifecycle = ?11, retention_policy = ?12, soft_delete_policy = ?13 WHERE name = ?1";

impl Catalog {
    pub(crate) async fn patch_bucket(
        &self,
        name: &str,
        update: BucketMetadataUpdate,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<BucketRecord, CatalogError> {
        self.update_bucket(name, update, conditions, access, BucketWrite::Patch)
            .await
    }

    pub(crate) async fn replace_bucket(
        &self,
        name: &str,
        update: BucketMetadataUpdate,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<BucketRecord, CatalogError> {
        self.update_bucket(name, update, conditions, access, BucketWrite::Replace)
            .await
    }

    pub(super) async fn update_bucket(
        &self,
        name: &str,
        update: BucketMetadataUpdate,
        conditions: BucketConditions,
        access: &BucketAccess,
        mode: BucketWrite,
    ) -> Result<BucketRecord, CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        let principals = self.0.principals.get().cloned();
        self.run(move |connection| {
            let transaction = sql::immediate(connection)?;
            let change = BucketChange {
                name: &name,
                update,
                conditions,
                access: &access,
                mode,
            };
            let record =
                update_in_transaction(&transaction, change, clock.as_ref(), principals.as_ref())?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }
}

/// One bucket metadata write.
pub(super) struct BucketChange<'a> {
    pub(super) name: &'a str,
    pub(super) update: BucketMetadataUpdate,
    pub(super) conditions: BucketConditions,
    pub(super) access: &'a BucketAccess,
    pub(super) mode: BucketWrite,
}

/// Applies `change` as a PATCH or PUT does, journal entry included, inside the caller's
/// transaction; the caller commits.
pub(super) fn update_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    change: BucketChange<'_>,
    clock: &dyn Clock,
    principals: Option<&principals::Profile>,
) -> Result<BucketRecord, CatalogError> {
    let BucketChange {
        name,
        mut update,
        conditions,
        access,
        mode,
    } = change;
    let mut record = select_bucket(transaction, name)?.ok_or(CatalogError::NotFound)?;
    check_access(transaction, name, &record.project, access)?;
    let pap = update
        .public_access_prevention_enforced
        .unwrap_or(record.public_access_prevention_enforced);
    if pap != record.public_access_prevention_enforced
        && let BucketAccess::Policy {
            identity, project, ..
        } = access
    {
        check_access(
            transaction,
            name,
            &record.project,
            &BucketAccess::Policy {
                identity: identity.clone(),
                project: project.clone(),
                permission: crate::auth::Permission::BucketsSetIam,
            },
        )?;
    }
    if !conditions.hold(record.metageneration) {
        return Err(CatalogError::PreconditionFailed);
    }
    // Cloud Storage answered a PATCH carrying only ignored members with the unchanged
    // resource: no metageneration advance and no new `updated` time (observed 2026-10-31).
    if matches!(mode, BucketWrite::Patch) && update.is_empty() {
        return Ok(record);
    }
    if update
        .location
        .as_ref()
        .is_some_and(|location| !location.eq_ignore_ascii_case(&record.location))
    {
        return Err(CatalogError::UnsupportedMetadata("location".into()));
    }
    apply_uniform_access(
        &mut record,
        update.uniform_bucket_level_access_enabled,
        clock.now(),
    )?;
    if update
        .object_retention_enabled
        .is_some_and(|enabled| enabled != record.object_retention_enabled)
    {
        return Err(CatalogError::InvalidMetadata("objectRetention can only be enabled when the bucket is created (enableObjectRetention=true) and can never be disabled; omit objectRetention".into()));
    }
    if let Some(retention) = update.soft_delete_retention {
        apply_soft_delete_retention(&mut record, retention, clock.now());
    }
    apply_retention_policy(
        &mut record,
        &update,
        conditions.if_metageneration_match,
        clock.now(),
    )?;
    validate_retention_expiration(transaction, &record)?;
    bucket_autoclass::update(transaction, &mut record, &update, clock.now())?;
    if let Some(labels) = update.labels.take() {
        apply_labels(&mut record, labels, mode, update.labels_mode)?;
    }
    if record.labels.len() > 64 {
        return Err(CatalogError::InvalidMetadata(
            "A bucket can have at most 64 labels".into(),
        ));
    }
    apply_plain_settings(&mut record, &mut update)?;
    record.metageneration = record
        .metageneration
        .checked_add(1)
        .ok_or_else(|| unavailable("bucket metageneration exhausted"))?;
    let updated = unix_micros(clock.now())?;
    record.updated = system_time_micros(updated)?;
    record.uniform_bucket_level_access_locked_time = record
        .uniform_bucket_level_access_locked_time
        .map(|time| {
            unix_micros(time).and_then(|time| system_time_micros(time).map_err(unavailable))
        })
        .transpose()?;
    store_bucket(transaction, &record, updated)?;
    for (target, entries) in [
        (acl::ResolvedAclTarget::Bucket(name), update.acl),
        (
            acl::ResolvedAclTarget::DefaultObject(name),
            update.default_object_acl,
        ),
    ] {
        if let Some(entries) = entries {
            acl::replace_resource_acl(transaction, target, &entries, access, principals)?;
        }
    }
    change_journal::append(
        transaction,
        change_journal::origin(access),
        change_journal::ChangeKind::BucketUpdate,
        name,
        None,
    )?;
    Ok(record)
}

fn store_bucket(
    transaction: &rusqlite::Transaction<'_>,
    record: &BucketRecord,
    updated: i64,
) -> Result<(), CatalogError> {
    transaction.prepare_cached(UPDATE_BUCKET)?.execute(params![
        record.name,
        serde_json::to_string(&record.labels).map_err(unavailable)?,
        record.storage_class,
        record.versioning_enabled,
        record.default_event_based_hold,
        record.metageneration,
        updated,
        cors_value(&record.cors).to_string(),
        record.public_access_prevention_enforced,
        record
            .uniform_bucket_level_access_locked_time
            .map(unix_micros)
            .transpose()?,
        serde_json::to_string(&record.lifecycle).map_err(unavailable)?,
        record
            .retention_policy
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(unavailable)?,
        record
            .soft_delete_policy
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(unavailable)?,
        record.versioning_configured,
    ])?;
    Ok(())
}

/// Whether a bucket metadata write merges into (PATCH) or replaces (PUT) stored labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BucketWrite {
    Patch,
    Replace,
}

fn apply_uniform_access(
    record: &mut BucketRecord,
    requested: Option<bool>,
    now: SystemTime,
) -> Result<(), CatalogError> {
    let Some(enabled) = requested else {
        return Ok(());
    };
    match (enabled, record.uniform_bucket_level_access_locked_time) {
        (true, None) => {
            record.uniform_bucket_level_access_locked_time =
                Some(now.checked_add(UNIFORM_ACCESS_LOCK_PERIOD).ok_or_else(|| {
                    CatalogError::InvalidMetadata(
                        "Uniform access deadline is outside the supported time range".into(),
                    )
                })?);
        }
        (false, Some(locked_time)) => {
            if now >= locked_time {
                return Err(CatalogError::Metadata(
                    crate::metadata_error::MetadataError::UniformAccessLocked,
                ));
            }
            record.uniform_bucket_level_access_locked_time = None;
        }
        _ => {}
    }
    Ok(())
}

fn apply_soft_delete_retention(
    record: &mut BucketRecord,
    retention: crate::bucket::SoftDeleteRetention,
    now: SystemTime,
) {
    let duration = retention.seconds();
    let effective_time = record
        .soft_delete_policy
        .as_ref()
        .filter(|policy| duration <= policy.retention_duration_seconds)
        .map_or(now, |policy| policy.effective_time);
    record.soft_delete_policy = Some(crate::bucket::SoftDeletePolicy {
        retention_duration_seconds: duration,
        effective_time,
    });
}

/// Locks, sets or clears the bucket retention policy; a locked policy never shrinks.
fn validate_retention_expiration(
    connection: &Connection,
    record: &BucketRecord,
) -> Result<(), CatalogError> {
    let Some(policy) = &record.retention_policy else {
        return Ok(());
    };
    let latest_creation = i64::try_from(
        253_402_300_799_999_999_i128 - i128::from(policy.retention_period) * 1_000_000,
    )
    .map_err(unavailable)?;
    let invalid: bool = sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND time_created > ?2)",
        rusqlite::params![record.name, latest_creation],
    )?;
    if invalid {
        return Err(CatalogError::InvalidMetadata("Bucket retention would place an existing object's expiration outside the supported RFC3339 UTC range".into()));
    }
    Ok(())
}

fn apply_retention_policy(
    record: &mut BucketRecord,
    update: &BucketMetadataUpdate,
    if_metageneration_match: Option<i64>,
    now: SystemTime,
) -> Result<(), CatalogError> {
    if update.lock_retention_policy {
        if if_metageneration_match.is_none() {
            return Err(CatalogError::InvalidMetadata(
                "Locking requires ifMetagenerationMatch; read the bucket metageneration first"
                    .into(),
            ));
        }
        let policy = record
            .retention_policy
            .as_mut()
            .ok_or(CatalogError::RetentionPolicyNotSet)?;
        if policy.is_locked {
            return Err(CatalogError::InvalidMetadata(
                "Retention policy is already locked".into(),
            ));
        }
        policy.is_locked = true;
        return Ok(());
    }
    let Some(period) = update.retention_period else {
        return Ok(());
    };
    if update.omitted_retention
        && record
            .retention_policy
            .as_ref()
            .is_some_and(|p| p.is_locked)
    {
        return Ok(());
    }
    if let Some(policy) = record
        .retention_policy
        .as_ref()
        .filter(|p| p.is_locked && period.is_none_or(|period| period < p.retention_period))
    {
        return Err(CatalogError::RetentionPolicyLocked(policy.retention_period));
    }
    let Some(period) = period else {
        record.retention_policy = None;
        return Ok(());
    };
    if !(1..3_155_760_000).contains(&period) {
        return Err(CatalogError::InvalidMetadata(
            "Retention policy must have a retention period greater than 0 and less than 100 years."
                .into(),
        ));
    }
    let locked = record
        .retention_policy
        .as_ref()
        .is_some_and(|p| p.is_locked);
    if record
        .retention_policy
        .as_ref()
        .is_none_or(|p| p.retention_period != period)
    {
        record.retention_policy = Some(crate::bucket::RetentionPolicy {
            retention_period: period,
            effective_time: now,
            is_locked: locked,
        });
    }
    Ok(())
}

/// Applies the settings that only validate and replace a stored value.
fn apply_plain_settings(
    record: &mut BucketRecord,
    update: &mut BucketMetadataUpdate,
) -> Result<(), CatalogError> {
    if let Some(class) = update.storage_class.take() {
        record.storage_class = class;
    }
    if let Some(enabled) = update.versioning_enabled {
        record.versioning_enabled = enabled;
    }
    if let Some(configured) = update.versioning_configured {
        record.versioning_configured = configured;
    }
    if let Some(hold) = update.default_event_based_hold {
        record.default_event_based_hold = hold;
    }
    if let Some(enforced) = update.public_access_prevention_enforced {
        record.public_access_prevention_enforced = enforced;
    }
    if let Some(cors) = update.cors.take() {
        validate_cors(&cors)?;
        record.cors = cors;
    }
    if let Some(lifecycle) = update.lifecycle.take() {
        record.lifecycle = lifecycle;
    }
    Ok(())
}

/// Applies a label update; `None` clears every label and a replace starts from none.
fn apply_labels(
    record: &mut BucketRecord,
    labels: Option<std::collections::BTreeMap<String, Option<String>>>,
    mode: BucketWrite,
    map_mode: crate::object::MapMode,
) -> Result<(), CatalogError> {
    if mode == BucketWrite::Replace || map_mode == crate::object::MapMode::Replace {
        record.labels.clear();
    }
    let Some(labels) = labels else {
        record.labels.clear();
        return Ok(());
    };
    for (key, value) in labels {
        // Library callers can bypass adapter validation.
        if ((mode == BucketWrite::Replace || map_mode == crate::object::MapMode::Replace)
            && value.is_none())
            || !valid_label(&key, LabelPart::Key)
            || value
                .as_deref()
                .is_some_and(|value| !valid_label(value, LabelPart::Value))
        {
            return Err(CatalogError::InvalidMetadata("Invalid bucket label".into()));
        }
        if let Some(value) = value {
            record.labels.insert(key, value);
        } else {
            record.labels.remove(&key);
        }
    }
    Ok(())
}
