use super::{
    Storage, convert, names,
    object_io::ObjectField,
    object_update::canned_acl,
    pb,
    routing::{Admitted, Route},
    status::{invalid_field, unsupported},
};
use crate::{
    acl::{AclRequest, AnonymousCannedAcl, refuse_anonymous_canned_acl},
    auth::{self, Caller},
    bucket::{BucketAccess, name::BucketKey},
    csek::{CopyKeys, CustomerKey},
    grpc::validate::{self, NegativeGeneration},
    object::{
        ComposeSource, ExpectedHashes, HashAlgorithm, ObjectConditions, ObjectError, ObjectKey,
        content_type, generation::Generation,
    },
    storage::{
        DerivedDestination, DerivedRequestGuard,
        rewrite::{RewriteBinding, RewriteError, RewriteStart, RewriteStep},
    },
};
use serde_json::{Map, Value};
use std::num::NonZeroU64;
use tonic::{Request, Response, Status};

/// A destination without the fields only the service supplies: the ones it keeps (the
/// generation, the etag, the retention expiry) and every field the proto marks `OUTPUT_ONLY`
/// ([`ObjectField::clear_service_set`]). ComposeObject composed a destination that carried any
/// one of the generation, the metageneration, the size, the etag, the creation time, the update
/// time, the checksums, the owner or the component count as if it had none (recorded in case
/// grpc-probe-inferred-destinations). That it ignores the other service-set fields, the
/// retention expiry among them, as well is inferred from the proto.
fn without_service_fields(mut o: pb::Object) -> pb::Object {
    ObjectField::clear_service_set(&mut o);
    o
}

/// RewriteObject's destination. It ignores the service-set fields like ComposeObject, except
/// the generation: a destination that carries one answers as if it named no object, while the
/// other fields set alone are ignored (recorded in case grpc-probe-inferred-destinations).
fn rewrite_destination(o: pb::Object) -> Result<pb::Object, Status> {
    if o.generation != 0 {
        return Err(invalid_field("You must specify an object name."));
    }
    Ok(without_service_fields(o))
}

fn validate_derived_fields(o: &pb::Object) -> Result<(), Status> {
    let supported = pb::Object {
        name: o.name.clone(),
        bucket: o.bucket.clone(),
        content_type: o.content_type.clone(),
        content_encoding: o.content_encoding.clone(),
        content_disposition: o.content_disposition.clone(),
        content_language: o.content_language.clone(),
        cache_control: o.cache_control.clone(),
        metadata: o.metadata.clone(),
        storage_class: o.storage_class.clone(),
        temporary_hold: o.temporary_hold,
        event_based_hold: o.event_based_hold,
        custom_time: o.custom_time,
        ..Default::default()
    };
    // Cloud Storage applied a destination `acl` on both RPCs (recorded 2026-11-18). Publication
    // gives a new generation the default object ACL or a canned one and a rewrite session
    // stores no entries, so listed entries stay refused instead of being dropped.
    if o != &supported {
        return Err(unsupported(
            "derived object fields; supply only editable JSON-supported metadata",
        ));
    }
    Ok(())
}

// Existing rewrite tokens bind this persisted representation; metadata application is typed.
fn derived_fields(o: &pb::Object) -> Result<Value, Status> {
    validate_derived_fields(o)?;
    let metadata = o
        .metadata
        .iter()
        .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
        .collect::<Map<_, _>>();
    let mut value = Map::from_iter([("metadata".into(), Value::Object(metadata))]);
    for (key, v) in [
        ("contentType", &o.content_type),
        ("contentEncoding", &o.content_encoding),
        ("contentDisposition", &o.content_disposition),
        ("contentLanguage", &o.content_language),
        ("cacheControl", &o.cache_control),
        ("storageClass", &o.storage_class),
    ] {
        if !v.is_empty() {
            value.insert(key.into(), v.clone().into());
        }
    }
    if o.temporary_hold {
        value.insert("temporaryHold".into(), true.into());
    }
    if let Some(v) = o.event_based_hold {
        value.insert("eventBasedHold".into(), v.into());
    }
    if let Some(v) = o.custom_time {
        value.insert("customTime".into(), v.to_string().into());
    }
    Ok(Value::Object(value))
}

fn rewrite_error(e: RewriteError) -> Status {
    match e {
        RewriteError::Invalid => Status::invalid_argument(
            "Invalid rewrite token; use the token from the preceding response",
        ),
        RewriteError::Gone => {
            Status::failed_precondition("Rewrite token expired or unavailable; restart the rewrite")
        }
        RewriteError::Object(e) => e.into(),
    }
}

/// The rewrite options a token binds, from their proto3 integers (0 is absent), and the keys
/// every call of the rewrite repeats.
#[derive(Clone, Copy, Debug)]
struct RewriteOptions {
    source_generation: Option<Generation>,
    max_bytes_per_call: Option<NonZeroU64>,
    keys: CopyKeys,
    expected: ExpectedHashes,
}

/// Cloud Storage's answer to a ComposeObject or RewriteObject checksum of `algorithm` that the
/// object it would write does not match: INVALID_ARGUMENT with a `BadRequest` repeating the
/// message (recorded 2026-11-13 and 2026-11-17, cases grpc-unsupported-options and
/// grpc-gaps-derived-checksums). Unlike the JSON message, it names neither digest.
fn checksum_mismatch(algorithm: HashAlgorithm) -> Status {
    invalid_field(algorithm.mismatch_message())
}

/// Cloud Storage's answer to an MD5 declared for an object that has none: it refused one on a
/// ComposeObject, whose result is composite, whether or not it matched (recorded 2026-11-17).
fn md5_unavailable() -> Status {
    invalid_field("Invalid argument.")
}

/// A failed ComposeObject, worded as Cloud Storage words a declared checksum the composed object
/// did not match. Damaged stored data stays a storage failure.
fn compose_failure(error: ObjectError) -> Status {
    if let ObjectError::HashMismatch(mismatch) = &error {
        return checksum_mismatch(mismatch.algorithm());
    }
    error.into()
}

/// The CRC32C a ComposeObject declares; a composite object has no MD5 to declare.
fn compose_checksums(checksums: Option<&pb::ObjectChecksums>) -> Result<ExpectedHashes, Status> {
    let Some(checksums) = checksums else {
        return Ok(ExpectedHashes::default());
    };
    if !checksums.md5_hash.is_empty() {
        return Err(md5_unavailable());
    }
    Ok(ExpectedHashes {
        md5: None,
        crc32c: checksums.crc32c,
    })
}

/// The digests a RewriteObject declares. Cloud Storage checked both against the source, and
/// named an MD5 of another length in base64 (recorded 2026-11-17).
fn rewrite_checksums(checksums: Option<&pb::ObjectChecksums>) -> Result<ExpectedHashes, Status> {
    let Some(checksums) = checksums else {
        return Ok(ExpectedHashes::default());
    };
    let md5 = match checksums.md5_hash.as_slice() {
        [] => None,
        digest => Some(<[u8; 16]>::try_from(digest).map_err(|_| {
            invalid_field(format!(
                "Provided value ({}) is not a base64-encoded 128-bit MD5 hash.",
                crate::base64::encode(digest)
            ))
        })?),
    };
    Ok(ExpectedHashes {
        md5,
        crc32c: checksums.crc32c,
    })
}

fn check_rewrite_request(r: &pb::RewriteObjectRequest) -> Result<RewriteOptions, Status> {
    let keys = super::customer_key::copy_keys(r)?;
    if !r.destination_kms_key.is_empty() {
        return Err(unsupported(
            "RewriteObject destination_kms_key; omit destination_kms_key",
        ));
    }
    let expected = rewrite_checksums(r.object_checksums.as_ref())?;
    let invalid = || {
        Status::invalid_argument(
            "source_generation must be positive or omitted; max_bytes_rewritten_per_call must be a multiple of 1048576",
        )
    };
    let source_generation = validate::selected_generation(r.source_generation)
        .map_err(|NegativeGeneration| invalid())?;
    let max_bytes_per_call = u64::try_from(r.max_bytes_rewritten_per_call)
        .ok()
        .filter(|bytes| bytes % 1_048_576 == 0)
        .ok_or_else(invalid)?;
    Ok(RewriteOptions {
        source_generation,
        max_bytes_per_call: NonZeroU64::new(max_bytes_per_call),
        keys,
        expected,
    })
}

/// The request parameters a rewrite token binds, in their JSON API names and decimal strings.
fn rewrite_parameters(r: &pb::RewriteObjectRequest, options: RewriteOptions) -> Map<String, Value> {
    let decimal = |value: Option<i64>| value.map(|value| value.to_string());
    let mut parameters = Map::new();
    for (key, value) in [
        (
            "sourceGeneration",
            options
                .source_generation
                .map(|generation| generation.to_string()),
        ),
        (
            "maxBytesRewrittenPerCall",
            options.max_bytes_per_call.map(|bytes| bytes.to_string()),
        ),
        ("ifGenerationMatch", decimal(r.if_generation_match)),
        ("ifGenerationNotMatch", decimal(r.if_generation_not_match)),
        ("ifMetagenerationMatch", decimal(r.if_metageneration_match)),
        (
            "ifMetagenerationNotMatch",
            decimal(r.if_metageneration_not_match),
        ),
        (
            "ifSourceGenerationMatch",
            decimal(r.if_source_generation_match),
        ),
        (
            "ifSourceGenerationNotMatch",
            decimal(r.if_source_generation_not_match),
        ),
        (
            "ifSourceMetagenerationMatch",
            decimal(r.if_source_metageneration_match),
        ),
        (
            "ifSourceMetagenerationNotMatch",
            decimal(r.if_source_metageneration_not_match),
        ),
        // Last, so the sessions stored before them keep their spelling. A continuation that
        // repeats the checksums is checked against the ones the first call verified.
        (
            "destinationPredefinedAcl",
            (!r.destination_predefined_acl.is_empty())
                .then(|| r.destination_predefined_acl.clone()),
        ),
        (
            "objectChecksums.crc32c",
            options.expected.crc32c.map(|crc32c| crc32c.to_string()),
        ),
        (
            "objectChecksums.md5Hash",
            options.expected.md5.map(|md5| crate::base64::encode(&md5)),
        ),
    ] {
        if let Some(value) = value {
            parameters.insert(key.to_owned(), Value::from(value));
        }
    }
    parameters
}

/// The canned ACL a ComposeObject or RewriteObject names for its destination, which
/// publication applies as it applies a JSON `destinationPredefinedAcl`; Cloud Storage applied
/// `private` on both RPCs (case grpc-unsupported-options, recorded 2026-11-13). An anonymous
/// caller cannot name one, as on a JSON upload: its object belongs to the project owners.
fn destination_acl(value: &str, access: &BucketAccess) -> Result<AclRequest, Status> {
    let Some(acl) = canned_acl("destination_predefined_acl", value)? else {
        return Ok(AclRequest::Omitted);
    };
    refuse_anonymous_canned_acl(
        access,
        AnonymousCannedAcl::Parameter("destination_predefined_acl"),
    )?;
    Ok(AclRequest::Predefined(acl))
}

impl From<AnonymousCannedAcl> for Status {
    fn from(error: AnonymousCannedAcl) -> Self {
        Self::invalid_argument(error.to_string())
    }
}

/// Source and destination of a rewrite; names a request omits come from its token's binding.
struct RewriteTarget<'a> {
    source: ObjectKey<'a>,
    destination: ObjectKey<'a>,
}

impl<'a> RewriteTarget<'a> {
    fn resolve(
        r: &'a pb::RewriteObjectRequest,
        binding: Option<&'a RewriteBinding>,
        profile: crate::profile::CompatProfile,
    ) -> Result<Self, Status> {
        let sb = if r.source_bucket.is_empty() {
            binding.map_or("", |b| b.source.bucket.as_str())
        } else {
            names::bucket_name(&r.source_bucket)?
        };
        let sn = if r.source_object.is_empty() {
            binding.map_or("", |b| b.source.name.as_str())
        } else {
            &r.source_object
        };
        let db = if r.destination_bucket.is_empty() {
            binding.map_or("", |b| b.destination.bucket.as_str())
        } else {
            names::bucket_name(&r.destination_bucket)?
        };
        let dn = if r.destination_name.is_empty() {
            binding.map_or("", |b| b.destination.name.as_str())
        } else {
            &r.destination_name
        };
        let sb = BucketKey::for_profile(sb, profile)?;
        let db = BucketKey::for_profile(db, profile)?;
        names::object(sn)?;
        names::object(dn)?;
        Ok(Self {
            source: ObjectKey::from_bucket(sb, sn),
            destination: ObjectKey::from_bucket(db, dn),
        })
    }

    fn matches(&self, binding: &RewriteBinding) -> bool {
        self.source == binding.source.key() && self.destination == binding.destination.key()
    }
}

/// A rewrite request without a token, after validation and name resolution.
struct NewRewrite<'a> {
    target: RewriteTarget<'a>,
    options: RewriteOptions,
    source_conditions: ObjectConditions,
    destination_conditions: ObjectConditions,
    parameters: Map<String, Value>,
    metadata: Option<Value>,
    resource: Option<&'a pb::Object>,
    acl: AclRequest,
}

impl Storage {
    pub(super) async fn rewrite_object(
        &self,
        request: Request<pb::RewriteObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::RewriteResponse>, Status> {
        let (_, extensions, mut r) = request.into_parts();
        let options = check_rewrite_request(&r)?;
        let sc = r.source_conditions();
        let dc = ObjectConditions::from(&r);
        r.destination = r.destination.take().map(rewrite_destination).transpose()?;
        let body = r.destination.as_ref().map(derived_fields).transpose()?;
        if r.destination
            .as_ref()
            .is_some_and(|o| !o.name.is_empty() || !o.bucket.is_empty())
        {
            return Err(Status::invalid_argument(
                "Rewrite destination metadata must omit name and bucket; use destination_name and destination_bucket",
            ));
        }
        let parameters = rewrite_parameters(&r, options);
        // The token is looked up before the routing comparison because a continuation may
        // leave out its buckets, and the session the token names supplies them; only then are
        // the source and destination known to compare.
        let session = if r.rewrite_token.is_empty() {
            None
        } else {
            Some(
                self.state
                    .catalog
                    .rewrite_step(&r.rewrite_token)
                    .await
                    .map_err(rewrite_error)?,
            )
        };
        let (step, session) = session.unzip();
        let binding = session.as_ref().map(|s| &s.binding);
        let target = RewriteTarget::resolve(&r, binding, self.compat_profile)?;
        let source_bucket = target.source.bucket().into_inner();
        let destination_bucket = target.destination.bucket().into_inner();
        Admitted::of(&extensions)?.compare(&[
            Route::SourceBucket(Some(source_bucket)),
            Route::DestinationBucket(Some(destination_bucket)),
        ])?;
        let read = self
            .access(caller, source_bucket, auth::OBJECTS_GET)
            .await?;
        let create = self
            .access(caller, destination_bucket, auth::OBJECTS_CREATE)
            .await?;
        // A new session's guard stays armed until its first response is stored and read back;
        // returning early drops it, which cancels the session.
        let (step, mut owner) = if let Some((step, session)) = step.zip(session.as_ref()) {
            let binding = &session.binding;
            if !target.matches(binding)
                || parameters
                    .iter()
                    .any(|(k, v)| binding.parameters.get(k) != Some(v))
                || body
                    .as_ref()
                    .is_some_and(|v| binding.metadata.as_ref() != Some(v))
            {
                return Err(Status::invalid_argument(
                    "rewrite_token does not match the request; repeat the original parameters",
                ));
            }
            // Neither the proto nor Cloud Storage's recorded answers say whether a
            // continuation may omit the keys; this follows the JSON API, which requires both.
            session.check_customer_keys(options.keys)?;
            (step, None)
        } else {
            let new = NewRewrite {
                target,
                options,
                source_conditions: sc,
                destination_conditions: dc,
                parameters,
                metadata: body,
                resource: r.destination.as_ref(),
                acl: destination_acl(&r.destination_predefined_acl, &create)?,
            };
            let (step, guard) = self.begin_rewrite(new, &read, &create).await?;
            (step, Some(guard))
        };
        let progress = self
            .state
            .catalog
            .advance_rewrite(step, read, create, super::stored::rewrite_resource)
            .await
            .map_err(rewrite_error)?;
        let mut resource = progress
            .resource
            .as_ref()
            .map(super::stored::from_rewrite_resource)
            .transpose()?;
        if let Some(owner) = &mut owner {
            owner.disarm();
        }
        if let Some(resource) = &mut resource {
            self.add_written_owner_fields(caller, destination_bucket, resource)
                .await?;
        }
        Ok(Response::new(pb::RewriteResponse {
            total_bytes_rewritten: crate::grpc::convert::signed(progress.offset, "rewrite offset")?,
            object_size: crate::grpc::convert::signed(progress.total, "rewrite total")?,
            done: progress.token.is_none(),
            rewrite_token: progress.token.unwrap_or_default(),
            resource,
        }))
    }

    /// Resolves the source and destination of a rewrite without a token and opens its session.
    async fn begin_rewrite(
        &self,
        new: NewRewrite<'_>,
        read: &BucketAccess,
        create: &BucketAccess,
    ) -> Result<(RewriteStep, DerivedRequestGuard), Status> {
        let NewRewrite {
            target:
                RewriteTarget {
                    source: source_key,
                    destination: destination_key,
                },
            options,
            source_conditions: sc,
            destination_conditions: dc,
            parameters,
            metadata: body,
            resource,
            acl,
        } = new;
        let generation = options.source_generation.map(Generation::get);
        let source = self
            .state
            .catalog
            .resolve_derived_source_keyed(
                source_key.into_target(generation),
                sc,
                read,
                options.keys.source,
            )
            .await?;
        // A rewrite reproduces the source's bytes, so the declared digests are checked against
        // its hashes before any session starts; a refused rewrite writes nothing. A source with
        // no MD5 to show, such as a composite one, refuses a declared MD5 as a compose does
        // (inferred: no rewrite of such a source was recorded).
        if options.expected.md5.is_some() && !source.object.has_md5() {
            return Err(md5_unavailable());
        }
        options
            .expected
            .verify(&source.object.hashes())
            .map_err(|mismatch| checksum_mismatch(mismatch.algorithm()))?;
        let mut metadata = resource.map_or_else(
            || super::object_update::inherited(&source.object),
            super::object_update::for_write,
        );
        // The destination key alone decides the new object's encryption; rewriting an object
        // onto itself with a new key rotates it.
        metadata.customer_key_sha256 = options.keys.destination.map(CustomerKey::sha256);
        // A hold and a custom time apply as on a JSON rewrite: Cloud Storage stored the custom
        // time (case grpc-unsupported-options, recorded 2026-11-13).
        metadata.acl = acl;
        super::object_update::validate(&metadata)?;
        let class = match resource.filter(|object| !object.storage_class.is_empty()) {
            Some(object) => Some(
                crate::bucket::validate_storage_class(&object.storage_class)
                    .map_err(super::bucket_update::input_error)?,
            ),
            None => metadata.storage_class.take(),
        };
        let destination_target = destination_key.into_target(());
        let buckets = self
            .state
            .catalog
            .rewrite_buckets(
                source_key.bucket().into_inner(),
                &destination_target,
                dc,
                create,
            )
            .await?
            .ok_or_else(|| Status::not_found("Source bucket not found"))?;
        let budget = buckets.budget(
            source.object.storage_class,
            class,
            options.max_bytes_per_call,
        );
        let content_type = metadata
            .content_type
            .value()
            .cloned()
            .unwrap_or_else(|| content_type::DEFAULT.to_owned());
        let destination = DerivedDestination {
            bucket: destination_key.bucket().into_inner().into(),
            name: destination_key.name().into(),
            content_type,
            metadata,
            storage_class: class,
            declared: ExpectedHashes::default(),
            recorded: source.object.hashes().into(),
            conditions: dc,
            access: create.clone(),
            component_count: source.object.component_count,
        };
        let start = RewriteStart {
            binding: RewriteBinding {
                source: source_key.into_target(source.object.generation.get()),
                destination: destination_target,
                parameters,
                metadata: body,
            },
            destination,
            source_conditions: sc,
            source_metageneration: source.object.metageneration.get(),
            source_access: read.clone(),
            source_key_sha256: options.keys.source.map(CustomerKey::sha256),
            budget,
        };
        self.state
            .catalog
            .begin_checked_rewrite(vec![source.whole()], self.max_object_bytes, start, buckets)
            .await
            .map_err(rewrite_error)
    }
    pub(super) async fn compose_object(
        &self,
        request: Request<pb::ComposeObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let mut r = request.into_inner();
        let key = super::customer_key::key(r.common_object_request_params.as_ref())?;
        if !r.kms_key.is_empty() {
            return Err(unsupported("ComposeObject kms_key; omit kms_key"));
        }
        let declared = compose_checksums(r.object_checksums.as_ref())?;
        let o = r.destination.take().ok_or_else(|| {
            Status::invalid_argument("destination is required; supply bucket and name")
        })?;
        let o = without_service_fields(o);
        let destination = self.object_key(&o.bucket, &o.name)?;
        let access = self
            .access(
                caller,
                destination.bucket().into_inner(),
                auth::OBJECTS_CREATE,
            )
            .await?;
        validate_derived_fields(&o)?;
        let mut metadata = super::object_update::for_write(&o);
        // The catalog applies this one key to every source and to the result.
        metadata.customer_key_sha256 = key.map(CustomerKey::sha256);
        metadata.acl = destination_acl(&r.destination_predefined_acl, &access)?;
        let names_acl = metadata.acl.predefined().is_some();
        let sources = r
            .source_objects
            .iter()
            .map(|s| {
                names::object(&s.name)?;
                let generation =
                    validate::selected_generation(s.generation).map_err(|NegativeGeneration| {
                        Status::invalid_argument("source generation must be positive or omitted")
                    })?;
                Ok(ComposeSource {
                    name: s.name.clone(),
                    generation: generation.map(Generation::get),
                    if_generation_match: s
                        .object_preconditions
                        .as_ref()
                        .and_then(|p| p.if_generation_match),
                })
            })
            .collect::<Result<Vec<_>, Status>>()?;
        // A hold and a custom time apply as on a JSON compose: Cloud Storage stored the custom
        // time (case grpc-unsupported-options, recorded 2026-11-13).
        // A destination class is accepted when every source has it; the catalog refuses any
        // other (recorded 2026-11-18).
        let storage_class = (!o.storage_class.is_empty())
            .then(|| crate::bucket::validate_storage_class(&o.storage_class))
            .transpose()
            .map_err(super::bucket_update::input_error)?;
        super::object_update::validate(&metadata)?;
        let destination = DerivedDestination {
            bucket: destination.bucket().into_inner().into(),
            name: destination.name().into(),
            content_type: content_type::or_default(o.content_type),
            metadata,
            storage_class,
            // The catalog checks a declared CRC32C against the one it combines from the sources
            // before reading them, so a refused compose leaves no object (recorded 2026-11-17,
            // case grpc-gaps-derived-checksums).
            declared,
            recorded: ExpectedHashes::default(),
            conditions: ObjectConditions::from(&r),
            access,
            component_count: None,
        };
        let record = self
            .state
            .catalog
            .compose_object(
                destination,
                sources,
                if r.delete_source_objects.unwrap_or(false) {
                    crate::storage::SourceDisposal::Delete
                } else {
                    crate::storage::SourceDisposal::Keep
                },
                self.max_object_bytes,
            )
            .await
            .map_err(compose_failure)?;
        // Cloud Storage answers ComposeObject without acl or owner, unlike WriteObject and
        // RewriteObject, unless it names a canned ACL (case grpc-unsupported-options).
        let hashes = crate::csek::StoredHashes::Revealed;
        let object = if names_acl {
            self.written_object(&record, hashes, caller).await?
        } else {
            convert::object(&record, hashes)?
        };
        Ok(Response::new(object))
    }
}

#[cfg(test)]
mod destination_fields;
#[cfg(test)]
mod object_keys;
#[cfg(test)]
mod request_refusals;
#[cfg(test)]
mod rewrite_keys;

#[cfg(test)]
mod tests {
    use super::super::test_support::{
        RESOURCE, grants, setup, stored_object, undispatched, upload,
    };
    use super::*;
    use crate::acl::PredefinedObjectAcl;
    use crate::grpc::test_support::{
        self as fixtures, assert_bad_request, assert_code, assert_unsupported,
    };
    use tonic::Code;

    /// Rewrite sessions store these parameters, and a continued call must produce the same
    /// ones to match its token, so their keys, order and spelling are persisted state.
    #[test]
    fn rewrite_tokens_bind_the_stored_parameter_spelling() {
        let parameters = |r: &pb::RewriteObjectRequest| {
            let options = check_rewrite_request(r).unwrap();
            Value::Object(rewrite_parameters(r, options)).to_string()
        };
        let full = pb::RewriteObjectRequest {
            source_generation: 7,
            max_bytes_rewritten_per_call: 2 * 1_048_576,
            if_generation_match: Some(0),
            if_generation_not_match: Some(2),
            if_metageneration_match: Some(3),
            if_metageneration_not_match: Some(4),
            if_source_generation_match: Some(5),
            if_source_generation_not_match: Some(6),
            if_source_metageneration_match: Some(8),
            if_source_metageneration_not_match: Some(9),
            ..Default::default()
        };
        assert_eq!(
            parameters(&full),
            r#"{"sourceGeneration":"7","maxBytesRewrittenPerCall":"2097152","ifGenerationMatch":"0","ifGenerationNotMatch":"2","ifMetagenerationMatch":"3","ifMetagenerationNotMatch":"4","ifSourceGenerationMatch":"5","ifSourceGenerationNotMatch":"6","ifSourceMetagenerationMatch":"8","ifSourceMetagenerationNotMatch":"9"}"#
        );
        // proto3's 0 is absent for the generation and the per-call limit.
        assert_eq!(parameters(&pb::RewriteObjectRequest::default()), "{}");
        let large = pb::RewriteObjectRequest {
            source_generation: i64::MAX,
            max_bytes_rewritten_per_call: i64::MAX - i64::MAX % 1_048_576,
            if_metageneration_match: Some(1),
            ..Default::default()
        };
        assert_eq!(
            parameters(&large),
            r#"{"sourceGeneration":"9223372036854775807","maxBytesRewrittenPerCall":"9223372036853727232","ifMetagenerationMatch":"1"}"#
        );
        // A canned ACL binds last, so sessions stored without one keep their spelling.
        let canned = pb::RewriteObjectRequest {
            if_generation_match: Some(0),
            destination_predefined_acl: "private".into(),
            ..Default::default()
        };
        assert_eq!(
            parameters(&canned),
            r#"{"ifGenerationMatch":"0","destinationPredefinedAcl":"private"}"#
        );
        // Declared checksums bind after it, for the same reason.
        let checked = pb::RewriteObjectRequest {
            destination_predefined_acl: "private".into(),
            object_checksums: Some(pb::ObjectChecksums {
                crc32c: Some(1),
                md5_hash: vec![0; 16],
            }),
            ..Default::default()
        };
        assert_eq!(
            parameters(&checked),
            r#"{"destinationPredefinedAcl":"private","objectChecksums.crc32c":"1","objectChecksums.md5Hash":"AAAAAAAAAAAAAAAAAAAAAA=="}"#
        );
    }

    /// A rewrite token also binds the destination fields, so their stored spelling is
    /// persisted state like the parameters above.
    #[test]
    fn rewrite_tokens_bind_the_stored_destination_spelling() {
        let fields = |o: &pb::Object| derived_fields(o).unwrap().to_string();
        assert_eq!(fields(&pb::Object::default()), r#"{"metadata":{}}"#);
        let destination = pb::Object {
            metadata: [("owner".to_owned(), "alice".to_owned())].into(),
            content_type: "text/plain".into(),
            temporary_hold: true,
            event_based_hold: Some(false),
            ..Default::default()
        };
        assert_eq!(
            fields(&destination),
            r#"{"metadata":{"owner":"alice"},"contentType":"text/plain","temporaryHold":true,"eventBasedHold":false}"#
        );
    }

    /// ComposeObject and RewriteObject store a destination hold and custom time as their JSON
    /// counterparts do; Cloud Storage stored the custom time on both (case
    /// grpc-unsupported-options, recorded 2026-11-13).
    #[tokio::test]
    async fn grpc_derived_writes_store_a_temporary_hold_and_custom_time() {
        let (s, _dir) = setup().await;
        upload(&s, "source", 4).await;
        let custom_time = Some(prost_types::Timestamp {
            seconds: 1_577_836_800,
            nanos: 0,
        });
        let destination = |name: &str| pb::Object {
            bucket: RESOURCE.into(),
            name: name.into(),
            temporary_hold: true,
            custom_time,
            ..Default::default()
        };
        let compose = pb::ComposeObjectRequest {
            destination: Some(destination("composed")),
            source_objects: vec![pb::compose_object_request::SourceObject {
                name: "source".into(),
                ..Default::default()
            }],
            destination_predefined_acl: "publicRead".into(),
            ..Default::default()
        };
        let composed = s
            .compose_object(Request::new(compose), None)
            .await
            .unwrap()
            .into_inner();
        // A canned ACL brings the owner view into the response, as Cloud Storage answered.
        assert!(
            grants(&composed).contains(&("allUsers", "READER")),
            "{composed:?}"
        );
        assert!(composed.owner.is_some());
        let rewrite = pb::RewriteObjectRequest {
            source_bucket: RESOURCE.into(),
            source_object: "source".into(),
            destination_bucket: RESOURCE.into(),
            destination_name: "rewritten".into(),
            destination: Some(pb::Object {
                bucket: String::new(),
                name: String::new(),
                ..destination("")
            }),
            ..Default::default()
        };
        s.rewrite_object(undispatched(rewrite), None).await.unwrap();
        for name in ["composed", "rewritten"] {
            let stored = stored_object(&s, name).await.unwrap();
            assert!(stored.temporary_hold.is_held(), "{name}");
            assert_eq!(
                stored
                    .custom_time
                    .as_ref()
                    .map(crate::object::CustomTime::as_str),
                Some("2020-01-01T00:00:00Z"),
                "{name}"
            );
        }
        s.state.catalog.close().await.unwrap();
    }

    /// An anonymous caller cannot name a destination's canned ACL, as on a JSON upload; other
    /// callers get the parsed value, and an empty field names none.
    #[test]
    fn destination_acls_refuse_anonymous_callers_and_unknown_values() {
        let anonymous = BucketAccess::Policy {
            identity: None,
            permission: auth::OBJECTS_CREATE,
            project: None,
        };
        assert_eq!(
            destination_acl("private", &BucketAccess::Permissive).unwrap(),
            AclRequest::Predefined(PredefinedObjectAcl::Private)
        );
        assert_eq!(
            destination_acl("", &anonymous).unwrap(),
            AclRequest::Omitted
        );
        for (value, access) in [
            ("private", &anonymous),
            ("publicReadWrite", &BucketAccess::Permissive),
        ] {
            assert_eq!(
                destination_acl(value, access).unwrap_err().code(),
                tonic::Code::InvalidArgument,
                "{value}"
            );
        }
    }

    /// ComposeObject and RewriteObject check a declared checksum against the object they would
    /// write and write nothing when it disagrees, as Cloud Storage did (cases
    /// grpc-unsupported-options and grpc-gaps-derived-checksums).
    #[tokio::test]
    async fn grpc_derived_writes_verify_request_checksums() {
        let (s, _dir) = setup().await;
        let source = upload(&s, "source", 4).await;
        let checksums = |crc32c: Option<u32>, md5: &[u8]| {
            Some(pb::ObjectChecksums {
                crc32c,
                md5_hash: md5.to_vec(),
            })
        };
        let compose = |name: &str, object_checksums| pb::ComposeObjectRequest {
            destination: Some(pb::Object {
                bucket: RESOURCE.into(),
                name: name.into(),
                ..Default::default()
            }),
            source_objects: vec![pb::compose_object_request::SourceObject {
                name: "source".into(),
                ..Default::default()
            }],
            object_checksums,
            ..Default::default()
        };
        let composed = s
            .compose_object(
                Request::new(compose("composed", checksums(Some(source.crc32c), &[]))),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(composed.checksums.unwrap().crc32c, Some(source.crc32c));
        let crc32c = HashAlgorithm::Crc32c.mismatch_message();
        for (object_checksums, message) in [
            (checksums(Some(source.crc32c ^ 1), &[]), crc32c),
            // A composite object has no MD5, so even the source's own is refused.
            (checksums(None, &source.md5), "Invalid argument."),
        ] {
            assert_bad_request(
                s.compose_object(Request::new(compose("refused", object_checksums)), None)
                    .await,
                message,
            );
        }
        assert!(stored_object(&s, "refused").await.is_none());

        let rewrite = |name: &str, object_checksums| pb::RewriteObjectRequest {
            source_bucket: RESOURCE.into(),
            source_object: "source".into(),
            destination_bucket: RESOURCE.into(),
            destination_name: name.into(),
            object_checksums,
            ..Default::default()
        };
        let rewritten = s
            .rewrite_object(
                undispatched(rewrite(
                    "rewritten",
                    checksums(Some(source.crc32c), &source.md5),
                )),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(rewritten.done);
        let mut wrong_md5 = source.md5;
        wrong_md5[0] ^= 1;
        for (object_checksums, message) in [
            (checksums(Some(source.crc32c ^ 1), &source.md5), crc32c),
            (
                checksums(Some(source.crc32c), &wrong_md5),
                HashAlgorithm::Md5.mismatch_message(),
            ),
            (
                checksums(None, &[0; 3]),
                "Provided value (AAAA) is not a base64-encoded 128-bit MD5 hash.",
            ),
        ] {
            assert_bad_request(
                s.rewrite_object(undispatched(rewrite("refused", object_checksums)), None)
                    .await,
                message,
            );
        }
        // The composite copy holds the same bytes, yet has no MD5 to match, as on a compose.
        let composite = pb::RewriteObjectRequest {
            source_object: "composed".into(),
            ..rewrite("refused", checksums(None, &source.md5))
        };
        assert_bad_request(
            s.rewrite_object(undispatched(composite), None).await,
            "Invalid argument.",
        );
        assert!(stored_object(&s, "refused").await.is_none());
        s.state.catalog.close().await.unwrap();
    }

    /// The legacy REGIONAL class lives only in a region. Cloud Storage refused it in a
    /// multi-region bucket on UpdateBucket and RewriteObject with a BadRequest, and served it in
    /// a regional one (recorded 2026-11-17, cases bkt-gaps-legacy-class-*).
    #[tokio::test]
    async fn grpc_regional_class_follows_the_bucket_location() {
        let (s, _dir) = setup().await;
        upload(&s, "source", 4).await;
        let regional = crate::bucket::BucketRecord {
            location: "us-central1".parse().unwrap(),
            storage_class: crate::bucket::StorageClass::Regional,
            ..crate::bucket::BucketRecord::for_test(
                "update-copy-regional",
                "local-project",
                std::time::SystemTime::now(),
            )
        };
        s.state.catalog.insert_bucket(regional).await.unwrap();
        let update = |bucket: &str| pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: format!("projects/_/buckets/{bucket}"),
                storage_class: "REGIONAL".into(),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["storage_class"]),
            ..Default::default()
        };
        let rewrite = |bucket: &str| pb::RewriteObjectRequest {
            source_bucket: RESOURCE.into(),
            source_object: "source".into(),
            destination_bucket: format!("projects/_/buckets/{bucket}"),
            destination_name: "rewritten".into(),
            destination: Some(pb::Object {
                storage_class: "REGIONAL".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let refused = "Project 471889570513 may not create storageClass REGIONAL buckets with locationConstraint US.";
        assert_bad_request(
            s.update_bucket(Request::new(update("update-copy-bucket")), None)
                .await,
            refused,
        );
        assert_bad_request(
            s.rewrite_object(undispatched(rewrite("update-copy-bucket")), None)
                .await,
            refused,
        );
        assert!(stored_object(&s, "rewritten").await.is_none());
        let updated = s
            .update_bucket(Request::new(update("update-copy-regional")), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(updated.storage_class, "REGIONAL");
        let rewritten = s
            .rewrite_object(undispatched(rewrite("update-copy-regional")), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(rewritten.resource.unwrap().storage_class, "REGIONAL");
        s.state.catalog.close().await.unwrap();
    }

    /// A rewrite token binds the checksums its first call verified: a continuation that names
    /// others is refused, and one that repeats them carries on.
    #[tokio::test]
    async fn grpc_rewrite_tokens_bind_declared_checksums() {
        let (s, _dir) = setup().await;
        let source = upload(&s, "source", 3 * 1024 * 1024).await;
        let declared = |crc32c| {
            Some(pb::ObjectChecksums {
                crc32c: Some(crc32c),
                md5_hash: Vec::new(),
            })
        };
        // Another location splits the rewrite into calls of the given size.
        let request = pb::RewriteObjectRequest {
            source_bucket: RESOURCE.into(),
            source_object: "source".into(),
            destination_bucket: "projects/_/buckets/update-copy-other".into(),
            destination_name: "target".into(),
            max_bytes_rewritten_per_call: 1024 * 1024,
            object_checksums: declared(source.crc32c),
            ..Default::default()
        };
        let first = s
            .rewrite_object(undispatched(request.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert!(!first.done);
        let continued = pb::RewriteObjectRequest {
            rewrite_token: first.rewrite_token,
            ..request
        };
        let other = pb::RewriteObjectRequest {
            object_checksums: declared(source.crc32c ^ 1),
            ..continued.clone()
        };
        let refused = assert_code(
            s.rewrite_object(undispatched(other), None).await,
            Code::InvalidArgument,
        );
        assert_eq!(
            refused.message(),
            "rewrite_token does not match the request; repeat the original parameters"
        );
        let second = s
            .rewrite_object(undispatched(continued), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(second.total_bytes_rewritten, 2 * 1024 * 1024);
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_derived_writes_reject_unsupported_options_without_creating_objects() {
        let (s, _dir) = setup().await;
        let source = upload(&s, "source", 4).await;
        let kms_key = "projects/p/locations/l/keyRings/r/cryptoKeys/k";
        let composed = pb::Object {
            bucket: RESOURCE.into(),
            name: "composed".into(),
            ..Default::default()
        };
        let compose = pb::ComposeObjectRequest {
            destination: Some(composed.clone()),
            source_objects: vec![pb::compose_object_request::SourceObject {
                name: "source".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let request = pb::ComposeObjectRequest {
            kms_key: kms_key.into(),
            ..compose.clone()
        };
        assert_unsupported(
            s.compose_object(Request::new(request), None).await,
            "ComposeObject kms_key",
        );
        let request = pb::ComposeObjectRequest {
            destination: Some(pb::Object {
                storage_class: "NEARLINE".into(),
                ..composed
            }),
            ..compose.clone()
        };
        // Recorded 2026-11-18: a class other than the sources' is a client error.
        assert_bad_request(
            s.compose_object(Request::new(request), None).await,
            "Cannot compose object with destination storage class NEARLINE from source components that have different storage classes (STANDARD).",
        );
        assert!(stored_object(&s, "composed").await.is_none());

        let rewrite = pb::RewriteObjectRequest {
            source_bucket: RESOURCE.into(),
            source_object: "source".into(),
            destination_bucket: RESOURCE.into(),
            destination_name: "rewritten".into(),
            ..Default::default()
        };
        let request = pb::RewriteObjectRequest {
            destination_kms_key: kms_key.into(),
            ..rewrite.clone()
        };
        assert_unsupported(
            s.rewrite_object(undispatched(request), None).await,
            "RewriteObject destination_kms_key",
        );
        assert!(stored_object(&s, "rewritten").await.is_none());

        let restore = pb::RestoreObjectRequest {
            bucket: RESOURCE.into(),
            object: "source".into(),
            generation: source.generation.get(),
            restore_token: "token".into(),
            ..Default::default()
        };
        assert_unsupported(
            s.restore_object(Request::new(restore.clone()), None).await,
            "RestoreObject ACL/token",
        );

        // Customer-supplied keys reach every derived write through the same request parameters.
        let customer_key = Some(pb::CommonObjectRequestParams {
            encryption_algorithm: "AES256".into(),
            ..Default::default()
        });
        let option = "customer-supplied encryption";
        let request = pb::RestoreObjectRequest {
            common_object_request_params: customer_key,
            restore_token: String::new(),
            ..restore
        };
        assert_unsupported(s.restore_object(Request::new(request), None).await, option);
        for name in ["composed", "rewritten"] {
            assert!(stored_object(&s, name).await.is_none(), "{name}");
        }
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_compose_deletes_its_sources_only_when_asked() {
        let (s, _dir) = setup().await;
        for name in ["first", "second"] {
            upload(&s, name, 2).await;
        }
        let compose = |destination: &str, delete_source_objects| pb::ComposeObjectRequest {
            destination: Some(pb::Object {
                bucket: RESOURCE.into(),
                name: destination.into(),
                ..Default::default()
            }),
            source_objects: ["first", "second"]
                .map(|name| pb::compose_object_request::SourceObject {
                    name: name.into(),
                    ..Default::default()
                })
                .into(),
            delete_source_objects,
            ..Default::default()
        };
        for keep in [None, Some(false)] {
            let kept = s
                .compose_object(Request::new(compose("kept", keep)), None)
                .await
                .unwrap()
                .into_inner();
            assert_eq!((kept.size, kept.component_count), (4, 2));
            for name in ["first", "second"] {
                assert!(stored_object(&s, name).await.is_some(), "{name}");
            }
        }
        let moved = s
            .compose_object(Request::new(compose("moved", Some(true))), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!((moved.size, moved.component_count), (4, 2));
        for name in ["first", "second"] {
            assert!(stored_object(&s, name).await.is_none(), "{name}");
        }
        assert!(stored_object(&s, "moved").await.is_some());
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_rewrite_continuation_and_compose_conditions() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let source = upload(&s, "source", 3 * 1024 * 1024).await;
        let request = pb::RewriteObjectRequest {
            source_bucket: resource.into(),
            source_object: "source".into(),
            source_generation: source.generation.get(),
            destination_bucket: "projects/_/buckets/update-copy-other".into(),
            destination_name: "target".into(),
            max_bytes_rewritten_per_call: 1024 * 1024,
            if_generation_match: Some(0),
            ..Default::default()
        };
        let first = s
            .rewrite_object(undispatched(request.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert!(!first.done);
        assert_eq!(first.total_bytes_rewritten, 1024 * 1024);
        assert!(first.resource.is_none());
        let mut mismatched = request.clone();
        mismatched.rewrite_token = first.rewrite_token.clone();
        mismatched.destination_name = "different".into();
        assert_code(
            s.rewrite_object(undispatched(mismatched), None).await,
            Code::InvalidArgument,
        );
        let next = pb::RewriteObjectRequest {
            rewrite_token: first.rewrite_token.clone(),
            ..Default::default()
        };
        let second = s
            .rewrite_object(undispatched(next.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(second.total_bytes_rewritten, 2 * 1024 * 1024);
        let replay = s
            .rewrite_object(undispatched(next), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(second, replay);
        let last = s
            .rewrite_object(
                undispatched(pb::RewriteObjectRequest {
                    rewrite_token: second.rewrite_token,
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(last.done);
        assert!(last.rewrite_token.is_empty());
        assert_eq!(last.resource.unwrap().size, 3 * 1024 * 1024);
        let destination = pb::Object {
            bucket: resource.into(),
            name: "composed".into(),
            ..Default::default()
        };
        let src = pb::compose_object_request::SourceObject {
            name: "source".into(),
            generation: source.generation.get(),
            object_preconditions: Some(
                pb::compose_object_request::source_object::ObjectPreconditions {
                    if_generation_match: Some(source.generation.get() + 1),
                },
            ),
        };
        let request = pb::ComposeObjectRequest {
            destination: Some(destination.clone()),
            source_objects: vec![src.clone()],
            ..Default::default()
        };
        assert_code(
            s.compose_object(Request::new(request), None).await,
            Code::FailedPrecondition,
        );
        let src = pb::compose_object_request::SourceObject {
            object_preconditions: None,
            ..src
        };
        let too_many = pb::ComposeObjectRequest {
            destination: Some(destination.clone()),
            source_objects: vec![src.clone(); 33],
            ..Default::default()
        };
        assert_code(
            s.compose_object(Request::new(too_many), None).await,
            Code::InvalidArgument,
        );
        let request = pb::ComposeObjectRequest {
            destination: Some(destination),
            source_objects: vec![src],
            if_generation_match: Some(0),
            ..Default::default()
        };
        let joined = s
            .compose_object(Request::new(request.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(joined.component_count, 1);
        assert_eq!(joined.size, source.size as i64);
        assert_code(
            s.compose_object(Request::new(request), None).await,
            Code::FailedPrecondition,
        );
        s.state.catalog.close().await.unwrap();
    }
}
