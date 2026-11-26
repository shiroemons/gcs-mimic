//! The domain errors the adapters answer, with an input for every branch of their mappings.
//!
//! The JSON, XML and Storage v2 tests render these same cases, so a change to one adapter's
//! answer shows next to the others'. The cases live here, below every adapter, because the
//! wording all three share and the backend causes the cases are built from belong to this module.
//! Each builder checks that its cases name every variant of its error through an exhaustive
//! match, so a new variant fails to compile until it is named, and then fails every adapter's
//! tests until it has a case.

use crate::cause::{BackendCause, BackendFailure, IoCause, MemoryBudget, MemoryExhausted};
use crate::{
    acl::ConflictingAcl,
    auth::{
        AuthError, AuthFailure, Denied, InvalidProjectId, policy_input::PolicyInputError,
        testable::UntestablePermission,
    },
    bounded_body::BodyError,
    bucket::{
        CatalogError, ClassLocationConflict, StorageClass,
        name::{InvalidBucketName, ProfileNameError},
    },
    csek::CsekError,
    hashing::InvalidDigest,
    match_glob::GlobError,
    metadata_error::MetadataError,
    object::{
        ComponentFailure, ComponentFailureKind, HashMismatch, HeldObject, HoldKind, ObjectError,
        RetainedObject, SoftDeletedRequest, unix_nanos::UnixNanos,
    },
    page_token::PageTokenError,
    percent::{MalformedEncoding, QueryError},
    storage::{ListQueryError, Overrun, ResumableError},
    time_format::TimeOutOfRange,
    url::UrlError,
};
use std::collections::BTreeSet;

/// One input of a mapping: a label unique within its error type, and the error.
pub(crate) struct Case<E> {
    pub(crate) label: &'static str,
    pub(crate) error: E,
    /// The cause an `object_backend_failure` event logs when the error is worded through
    /// [`super::wording`], if it logs one.
    pub(crate) logged: Option<&'static str>,
}

pub(crate) fn case<E: std::error::Error + 'static>(label: &'static str, error: E) -> Case<E> {
    crate::error::assert_distinct_causes(&error);
    mapping_case(label, error)
}

/// Also used for mapping inputs that have no standard error chain.
fn mapping_case<E>(label: &'static str, error: E) -> Case<E> {
    Case {
        label,
        error,
        logged: None,
    }
}

impl<E> Case<E> {
    fn logging(mut self, cause: &'static str) -> Self {
        self.logged = Some(cause);
        self
    }
}

/// Checks that `cases` hold every variant `name` knows and returns them. `name` matches
/// exhaustively, so a new variant fails to compile there until it joins `all` and gets a case.
pub(crate) fn every_variant<E>(
    cases: Vec<Case<E>>,
    name: fn(&E) -> &'static str,
    all: &[&'static str],
) -> Vec<Case<E>> {
    let labels: BTreeSet<&str> = cases.iter().map(|case| case.label).collect();
    assert_eq!(labels.len(), cases.len(), "a label names two cases");
    let named: BTreeSet<&str> = cases.iter().map(|case| name(&case.error)).collect();
    assert_eq!(named, all.iter().copied().collect::<BTreeSet<_>>());
    cases
}

/// Each case's error `N` times, for renderings that each consume one: the domain errors are not
/// `Clone`, so the builder runs once per copy.
pub(crate) fn copies<E, const N: usize>(
    cases: fn() -> Vec<Case<E>>,
) -> Vec<(&'static str, [E; N])> {
    let mut builds: [std::vec::IntoIter<Case<E>>; N] = std::array::from_fn(|_| cases().into_iter());
    let mut rows = Vec::new();
    loop {
        let next = builds.each_mut().map(Iterator::next);
        let Some(first) = next.iter().flatten().next().map(|case| case.label) else {
            return rows;
        };
        rows.push((
            first,
            next.map(|copy| {
                let copy = copy.expect("every build has the same cases");
                assert_eq!(copy.label, first);
                copy.error
            }),
        ));
    }
}

/// A refusal of `storage.buckets.get` on `bucket` to `id`, or to the anonymous caller.
pub(crate) fn denied(id: Option<&str>) -> Box<Denied> {
    let identity = id.map(|id| {
        crate::auth::Identity::from_upload_snapshot(&serde_json::json!({"id": id, "projects": {}}))
            .unwrap()
    });
    Denied::new(
        identity.as_ref(),
        crate::auth::BUCKETS_GET,
        crate::auth::denied::Resource::in_bucket(crate::auth::BUCKETS_GET, "bucket"),
    )
}

pub(crate) const VIEWER: &str = "viewer@example.invalid";

/// A regional class in a multi-region, which the XML and Storage v2 adapters answer with their
/// own code and details.
pub(crate) fn class_location() -> MetadataError {
    let multi = "US".parse::<crate::bucket::Location>().unwrap();
    MetadataError::ClassLocation(
        ClassLocationConflict::check(StorageClass::Regional, multi, "123456").unwrap_err(),
    )
}

fn memory_full(budget: MemoryBudget) -> IoCause {
    MemoryExhausted::new(budget).into()
}

const UNCLASSIFIED: &str = "unrecognized backend failure (private details withheld)";

/// Names each variant. A new variant fails to compile here: give it a case in
/// [`object_cases`] and its name in [`OBJECT_VARIANTS`].
pub(crate) fn object_variant(error: &ObjectError) -> &'static str {
    match error {
        ObjectError::BucketNotFound => "BucketNotFound",
        ObjectError::ObjectNotFound => "ObjectNotFound",
        ObjectError::PreconditionFailed => "PreconditionFailed",
        ObjectError::Component(_) => "Component",
        ObjectError::PublicAccessPrevention => "PublicAccessPrevention",
        ObjectError::OverwriteForbidden(_) => "OverwriteForbidden",
        ObjectError::ActiveHold(_) => "ActiveHold",
        ObjectError::ObjectRetentionNotMet(_) => "ObjectRetentionNotMet",
        ObjectError::UnlockedRetentionOverrideRequired => "UnlockedRetentionOverrideRequired",
        ObjectError::LockedRetention => "LockedRetention",
        ObjectError::ObjectNotSoftDeleted => "ObjectNotSoftDeleted",
        ObjectError::SoftDeletePolicyRequired(_) => "SoftDeletePolicyRequired",
        ObjectError::Denied(_) => "Denied",
        ObjectError::Forbidden => "Forbidden",
        ObjectError::NotModified => "NotModified",
        ObjectError::WriteNotModified => "WriteNotModified",
        ObjectError::SourceNotModified => "SourceNotModified",
        ObjectError::ChecksumMismatch => "ChecksumMismatch",
        ObjectError::HashMismatch(_) => "HashMismatch",
        ObjectError::TooLarge => "TooLarge",
        ObjectError::InvalidFixedMetadata(_) => "InvalidFixedMetadata",
        ObjectError::UniformAccessPredefinedAcl => "UniformAccessPredefinedAcl",
        ObjectError::UniformAccessAclUpdate => "UniformAccessAclUpdate",
        ObjectError::Metadata(_) => "Metadata",
        ObjectError::InvalidDerivedArgument(_) => "InvalidDerivedArgument",
        ObjectError::ComposeClassMismatch { .. } => "ComposeClassMismatch",
        ObjectError::DerivedTransferNotFound => "DerivedTransferNotFound",
        ObjectError::DerivedTransferExpired => "DerivedTransferExpired",
        ObjectError::DerivedTransferBusy => "DerivedTransferBusy",
        ObjectError::DerivedTransferIncomplete => "DerivedTransferIncomplete",
        ObjectError::UnsupportedMetadata(_) => "UnsupportedMetadata",
        ObjectError::CustomerEncryption(_) => "CustomerEncryption",
        ObjectError::BodyAborted(_) => "BodyAborted",
        ObjectError::Io(_) => "Io",
        ObjectError::Corrupt(_) => "Corrupt",
        ObjectError::Unavailable(_) => "Unavailable",
    }
}

pub(crate) const OBJECT_VARIANTS: [&str; 36] = [
    "BucketNotFound",
    "ObjectNotFound",
    "PreconditionFailed",
    "Component",
    "PublicAccessPrevention",
    "OverwriteForbidden",
    "ActiveHold",
    "ObjectRetentionNotMet",
    "UnlockedRetentionOverrideRequired",
    "LockedRetention",
    "ObjectNotSoftDeleted",
    "SoftDeletePolicyRequired",
    "Denied",
    "Forbidden",
    "NotModified",
    "WriteNotModified",
    "SourceNotModified",
    "ChecksumMismatch",
    "HashMismatch",
    "TooLarge",
    "InvalidFixedMetadata",
    "UniformAccessPredefinedAcl",
    "UniformAccessAclUpdate",
    "Metadata",
    "InvalidDerivedArgument",
    "ComposeClassMismatch",
    "DerivedTransferNotFound",
    "DerivedTransferExpired",
    "DerivedTransferBusy",
    "DerivedTransferIncomplete",
    "UnsupportedMetadata",
    "CustomerEncryption",
    "BodyAborted",
    "Io",
    "Corrupt",
    "Unavailable",
];

/// Every object failure, with an input for each branch the adapters word differently.
pub(crate) fn object_cases() -> Vec<Case<ObjectError>> {
    let component = |kind| {
        ObjectError::Component(Box::new(ComponentFailure {
            bucket: "b".into(),
            name: "a".into(),
            kind,
        }))
    };
    let held = |kind| {
        ObjectError::ActiveHold(Box::new(HeldObject {
            bucket: "b".into(),
            name: "o".into(),
            kind,
        }))
    };
    let cases = vec![
        case("BucketNotFound", ObjectError::BucketNotFound),
        case("ObjectNotFound", ObjectError::ObjectNotFound),
        case("PreconditionFailed", ObjectError::PreconditionFailed),
        case(
            "Component(NotFound)",
            component(ComponentFailureKind::NotFound),
        ),
        case(
            "Component(GenerationMismatch)",
            component(ComponentFailureKind::GenerationMismatch {
                required: 1,
                actual: 7,
            }),
        ),
        case(
            "PublicAccessPrevention",
            ObjectError::PublicAccessPrevention,
        ),
        case(
            "OverwriteForbidden",
            ObjectError::OverwriteForbidden(denied(Some(VIEWER))),
        ),
        case("ActiveHold(Temporary)", held(HoldKind::Temporary)),
        case("ActiveHold(EventBased)", held(HoldKind::EventBased)),
        case(
            "ObjectRetentionNotMet",
            ObjectError::ObjectRetentionNotMet(Box::new(RetainedObject {
                bucket: "b".into(),
                name: "o".into(),
                until: UnixNanos::new(1_700_000_000, 500_000_000).unwrap(),
            })),
        ),
        case(
            "UnlockedRetentionOverrideRequired",
            ObjectError::UnlockedRetentionOverrideRequired,
        ),
        case("LockedRetention", ObjectError::LockedRetention),
        case("ObjectNotSoftDeleted", ObjectError::ObjectNotSoftDeleted),
        case(
            "SoftDeletePolicyRequired(List)",
            ObjectError::SoftDeletePolicyRequired(SoftDeletedRequest::List),
        ),
        case(
            "SoftDeletePolicyRequired(Get)",
            ObjectError::SoftDeletePolicyRequired(SoftDeletedRequest::Get),
        ),
        case(
            "SoftDeletePolicyRequired(Restore)",
            ObjectError::SoftDeletePolicyRequired(SoftDeletedRequest::Restore),
        ),
        case("Denied", ObjectError::Denied(denied(Some(VIEWER)))),
        case("Denied(anonymous)", ObjectError::Denied(denied(None))),
        case("Forbidden", ObjectError::Forbidden),
        case("NotModified", ObjectError::NotModified),
        case("WriteNotModified", ObjectError::WriteNotModified),
        case("SourceNotModified", ObjectError::SourceNotModified),
        case("ChecksumMismatch", ObjectError::ChecksumMismatch),
        case(
            "HashMismatch(Crc32c)",
            ObjectError::HashMismatch(Box::new(HashMismatch::Crc32c {
                expected: 1,
                computed: 2,
            })),
        ),
        case(
            "HashMismatch(Md5)",
            ObjectError::HashMismatch(Box::new(HashMismatch::Md5 {
                expected: [0; 16],
                computed: [1; 16],
            })),
        ),
        case("TooLarge", ObjectError::TooLarge),
        case(
            "InvalidFixedMetadata",
            ObjectError::InvalidFixedMetadata("Invalid contentType header value".into()),
        ),
        case(
            "UniformAccessPredefinedAcl",
            ObjectError::UniformAccessPredefinedAcl,
        ),
        case(
            "UniformAccessAclUpdate",
            ObjectError::UniformAccessAclUpdate,
        ),
        case(
            "Metadata(ClassLocation)",
            ObjectError::Metadata(class_location()),
        ),
        case(
            "Metadata(StorageClassRequiresZone)",
            ObjectError::Metadata(MetadataError::StorageClassRequiresZone),
        ),
        case(
            "Metadata(LongLanguage)",
            ObjectError::Metadata(MetadataError::LongLanguage),
        ),
        case(
            "InvalidDerivedArgument",
            ObjectError::InvalidDerivedArgument("sourceObjects must not be empty".into()),
        ),
        case(
            "ComposeClassMismatch",
            ObjectError::ComposeClassMismatch {
                destination: StorageClass::Standard,
                sources: vec![StorageClass::Nearline, StorageClass::Coldline],
            },
        ),
        case(
            "DerivedTransferNotFound",
            ObjectError::DerivedTransferNotFound,
        ),
        case(
            "DerivedTransferExpired",
            ObjectError::DerivedTransferExpired,
        ),
        case("DerivedTransferBusy", ObjectError::DerivedTransferBusy),
        case(
            "DerivedTransferIncomplete",
            ObjectError::DerivedTransferIncomplete,
        ),
        case(
            "UnsupportedMetadata",
            ObjectError::UnsupportedMetadata("metadata".into()),
        ),
        case(
            "CustomerEncryption(KeyIncorrect)",
            ObjectError::CustomerEncryption(CsekError::KeyIncorrect),
        ),
        case(
            "BodyAborted",
            ObjectError::BodyAborted(BackendCause::new("client disconnected")),
        ),
        case(
            "Io(StorageFull)",
            ObjectError::Io(std::io::ErrorKind::StorageFull.into()),
        ),
        case(
            "Io(memory blobs)",
            ObjectError::Io(memory_full(MemoryBudget::Blobs)),
        ),
        case(
            "Io(memory catalog)",
            ObjectError::Io(memory_full(MemoryBudget::Catalog)),
        ),
        case(
            "Io(PermissionDenied)",
            ObjectError::Io(std::io::ErrorKind::PermissionDenied.into()),
        ),
        case(
            "Io(with context)",
            super::object_context(
                "blob 7",
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ),
        ),
        case(
            "Corrupt(CatalogClosed)",
            ObjectError::Corrupt(BackendFailure::CatalogClosed.into()),
        ),
        case(
            "Corrupt(private cause)",
            ObjectError::Corrupt("invalid opaque blob ID: /secret token=private".into()),
        )
        .logging("invalid opaque blob ID"),
        case(
            "Unavailable(CatalogClosed)",
            ObjectError::Unavailable(BackendFailure::CatalogClosed.into()),
        ),
        case(
            "Unavailable(MissingCompletion)",
            ObjectError::Unavailable(BackendFailure::MissingCompletion.into()),
        )
        .logging(UNCLASSIFIED),
        case(
            "Unavailable(private cause)",
            ObjectError::Unavailable("database is locked: /secret token=private".into()),
        )
        .logging("database is locked"),
    ];
    every_variant(cases, object_variant, &OBJECT_VARIANTS)
}

/// Names each variant. A new variant fails to compile here: give it a case in
/// [`catalog_cases`] and its name in [`CATALOG_VARIANTS`].
pub(crate) fn catalog_variant(error: &CatalogError) -> &'static str {
    match error {
        CatalogError::AlreadyExists => "AlreadyExists",
        CatalogError::NotFound => "NotFound",
        CatalogError::NotEmpty => "NotEmpty",
        CatalogError::RestoreConflict => "RestoreConflict",
        CatalogError::PreconditionFailed => "PreconditionFailed",
        CatalogError::NotModified => "NotModified",
        CatalogError::PublicAccessPrevention => "PublicAccessPrevention",
        CatalogError::Denied(_) => "Denied",
        CatalogError::Forbidden => "Forbidden",
        CatalogError::RetentionPolicyLocked(_) => "RetentionPolicyLocked",
        CatalogError::RetentionPolicyNotSet => "RetentionPolicyNotSet",
        CatalogError::InvalidMetadata(_) => "InvalidMetadata",
        CatalogError::Metadata(_) => "Metadata",
        CatalogError::UnsupportedMetadata(_) => "UnsupportedMetadata",
        CatalogError::Io(_) => "Io",
        CatalogError::Corrupt(_) => "Corrupt",
        CatalogError::NewerSchema { .. } => "NewerSchema",
        CatalogError::Unavailable(_) => "Unavailable",
    }
}

pub(crate) const CATALOG_VARIANTS: [&str; 18] = [
    "AlreadyExists",
    "NotFound",
    "NotEmpty",
    "RestoreConflict",
    "PreconditionFailed",
    "NotModified",
    "PublicAccessPrevention",
    "Denied",
    "Forbidden",
    "RetentionPolicyLocked",
    "RetentionPolicyNotSet",
    "InvalidMetadata",
    "Metadata",
    "UnsupportedMetadata",
    "Io",
    "Corrupt",
    "NewerSchema",
    "Unavailable",
];

/// Every bucket catalog failure, with an input for each branch the adapters word differently.
pub(crate) fn catalog_cases() -> Vec<Case<CatalogError>> {
    let cases = vec![
        case("AlreadyExists", CatalogError::AlreadyExists),
        case("NotFound", CatalogError::NotFound),
        case("NotEmpty", CatalogError::NotEmpty),
        case("RestoreConflict", CatalogError::RestoreConflict),
        case("PreconditionFailed", CatalogError::PreconditionFailed),
        case("NotModified", CatalogError::NotModified),
        case(
            "PublicAccessPrevention",
            CatalogError::PublicAccessPrevention,
        ),
        case("Denied", CatalogError::Denied(denied(Some(VIEWER)))),
        case("Denied(anonymous)", CatalogError::Denied(denied(None))),
        case("Forbidden", CatalogError::Forbidden),
        case(
            "RetentionPolicyLocked",
            CatalogError::RetentionPolicyLocked(3600),
        ),
        case("RetentionPolicyNotSet", CatalogError::RetentionPolicyNotSet),
        case(
            "InvalidMetadata",
            CatalogError::InvalidMetadata("Invalid value for: x is not a valid value".into()),
        ),
        case(
            "Metadata(ClassLocation)",
            CatalogError::Metadata(class_location()),
        ),
        case(
            "Metadata(StorageClassRequiresZone)",
            CatalogError::Metadata(MetadataError::StorageClassRequiresZone),
        ),
        case(
            "Metadata(AutoclassTerminal)",
            CatalogError::Metadata(MetadataError::AutoclassTerminal),
        ),
        case(
            "UnsupportedMetadata",
            CatalogError::UnsupportedMetadata("cors".into()),
        ),
        case(
            "Io(StorageFull)",
            CatalogError::Io(std::io::ErrorKind::StorageFull.into()),
        ),
        case(
            "Io(memory catalog)",
            CatalogError::Io(memory_full(MemoryBudget::Catalog)),
        ),
        case(
            "Io(PermissionDenied)",
            CatalogError::Io(std::io::ErrorKind::PermissionDenied.into()),
        )
        .logging(UNCLASSIFIED),
        case(
            "Corrupt(CatalogClosed)",
            CatalogError::Corrupt(BackendFailure::CatalogClosed.into()),
        ),
        case(
            "Corrupt(private cause)",
            CatalogError::Corrupt("invalid opaque blob ID: /secret token=private".into()),
        )
        .logging("invalid opaque blob ID"),
        case(
            "NewerSchema",
            CatalogError::NewerSchema {
                version: 99,
                supported: 50,
            },
        )
        .logging(UNCLASSIFIED),
        case(
            "Unavailable(CatalogClosed)",
            CatalogError::Unavailable(BackendFailure::CatalogClosed.into()),
        ),
        case(
            "Unavailable(private cause)",
            CatalogError::Unavailable("database is locked: /secret token=private".into()),
        )
        .logging("database is locked"),
    ];
    every_variant(cases, catalog_variant, &CATALOG_VARIANTS)
}

pub(crate) fn auth_failure_cases() -> Vec<Case<AuthFailure>> {
    every_variant(
        vec![
            case("LoginRequired", AuthFailure::LoginRequired),
            case("InvalidCredentials", AuthFailure::InvalidCredentials),
            case("Denied", AuthFailure::Denied(denied(Some(VIEWER)))),
            case("Denied(anonymous)", AuthFailure::Denied(denied(None))),
            case("Forbidden", AuthFailure::Forbidden),
        ],
        |error| match error {
            AuthFailure::LoginRequired => "LoginRequired",
            AuthFailure::InvalidCredentials => "InvalidCredentials",
            AuthFailure::Denied(_) => "Denied",
            AuthFailure::Forbidden => "Forbidden",
        },
        &["LoginRequired", "InvalidCredentials", "Denied", "Forbidden"],
    )
}

pub(crate) fn auth_error_cases() -> Vec<Case<AuthError>> {
    every_variant(
        vec![
            case(
                "Failure(InvalidCredentials)",
                AuthError::Failure(AuthFailure::InvalidCredentials),
            ),
            case(
                "Unavailable(CatalogClosed)",
                AuthError::Unavailable(BackendFailure::CatalogClosed.into()),
            ),
            case(
                "Unavailable(private cause)",
                AuthError::Unavailable("database is locked: /secret token=private".into()),
            )
            .logging("database is locked"),
        ],
        |error| match error {
            AuthError::Failure(_) => "Failure",
            AuthError::Unavailable(_) => "Unavailable",
        },
        &["Failure", "Unavailable"],
    )
}

pub(crate) fn csek_cases() -> Vec<Case<CsekError>> {
    every_variant(
        vec![
            mapping_case("AlgorithmInvalid", CsekError::AlgorithmInvalid),
            mapping_case("KeyFormatInvalid", CsekError::KeyFormatInvalid),
            mapping_case("KeySha256Invalid", CsekError::KeySha256Invalid),
            mapping_case("KeyIncorrect", CsekError::KeyIncorrect),
            mapping_case("ResourceIsEncrypted", CsekError::ResourceIsEncrypted),
            mapping_case("ResourceNotEncrypted", CsekError::ResourceNotEncrypted),
        ],
        |error| match error {
            CsekError::AlgorithmInvalid => "AlgorithmInvalid",
            CsekError::KeyFormatInvalid => "KeyFormatInvalid",
            CsekError::KeySha256Invalid => "KeySha256Invalid",
            CsekError::KeyIncorrect => "KeyIncorrect",
            CsekError::ResourceIsEncrypted => "ResourceIsEncrypted",
            CsekError::ResourceNotEncrypted => "ResourceNotEncrypted",
        },
        &[
            "AlgorithmInvalid",
            "KeyFormatInvalid",
            "KeySha256Invalid",
            "KeyIncorrect",
            "ResourceIsEncrypted",
            "ResourceNotEncrypted",
        ],
    )
}

pub(crate) fn resumable_cases() -> Vec<Case<ResumableError>> {
    every_variant(
        vec![
            case("NotFound", ResumableError::NotFound),
            case("Gone", ResumableError::Gone),
            case("Cancelled", ResumableError::Cancelled),
            case("InvalidContentRange", ResumableError::InvalidContentRange),
            case(
                "Overrun",
                ResumableError::Overrun(Overrun {
                    received: 10,
                    room: 4,
                }),
            ),
            case(
                "Invalid",
                ResumableError::Invalid("Upload offset 7 does not match the session".into()),
            ),
            case(
                "Object(PreconditionFailed)",
                ResumableError::Object(ObjectError::PreconditionFailed),
            ),
        ],
        |error| match error {
            ResumableError::NotFound => "NotFound",
            ResumableError::Gone => "Gone",
            ResumableError::Cancelled => "Cancelled",
            ResumableError::InvalidContentRange => "InvalidContentRange",
            ResumableError::Overrun(_) => "Overrun",
            ResumableError::Invalid(_) => "Invalid",
            ResumableError::Object(_) => "Object",
        },
        &[
            "NotFound",
            "Gone",
            "Cancelled",
            "InvalidContentRange",
            "Overrun",
            "Invalid",
            "Object",
        ],
    )
}

pub(crate) fn body_cases() -> Vec<Case<BodyError>> {
    every_variant(
        vec![
            case("TooLarge", BodyError::TooLarge),
            case("OutOfMemory", BodyError::OutOfMemory),
            case("Unreadable", BodyError::Unreadable),
        ],
        |error| match error {
            BodyError::TooLarge => "TooLarge",
            BodyError::OutOfMemory => "OutOfMemory",
            BodyError::Unreadable => "Unreadable",
        },
        &["TooLarge", "OutOfMemory", "Unreadable"],
    )
}

pub(crate) fn page_token_cases() -> Vec<Case<PageTokenError>> {
    every_variant(
        vec![
            case("BucketCursor", PageTokenError::BucketCursor),
            case(
                "ObjectCursor(named)",
                PageTokenError::ObjectCursor(Some("CgFh".into())),
            ),
            case("ObjectCursor(unnamed)", PageTokenError::ObjectCursor(None)),
            case("Project", PageTokenError::Project(InvalidProjectId)),
            case(
                "BucketName",
                PageTokenError::BucketName(ProfileNameError::FakeGcsServer),
            ),
        ],
        |error| match error {
            PageTokenError::BucketCursor => "BucketCursor",
            PageTokenError::ObjectCursor(_) => "ObjectCursor",
            PageTokenError::Project(_) => "Project",
            PageTokenError::BucketName(_) => "BucketName",
        },
        &["BucketCursor", "ObjectCursor", "Project", "BucketName"],
    )
}

pub(crate) fn bucket_name_cases() -> Vec<Case<ProfileNameError>> {
    every_variant(
        vec![
            case("Native", ProfileNameError::Native(InvalidBucketName)),
            case("FakeGcsServer", ProfileNameError::FakeGcsServer),
        ],
        |error| match error {
            ProfileNameError::Native(_) => "Native",
            ProfileNameError::FakeGcsServer => "FakeGcsServer",
        },
        &["Native", "FakeGcsServer"],
    )
}

pub(crate) fn query_cases() -> Vec<Case<QueryError>> {
    every_variant(
        vec![
            case("TooLong", QueryError::TooLong),
            case("Duplicate", QueryError::Duplicate("prefix".into())),
            case("Malformed", QueryError::Malformed(MalformedEncoding)),
        ],
        |error| match error {
            QueryError::TooLong => "TooLong",
            QueryError::Duplicate(_) => "Duplicate",
            QueryError::Malformed(_) => "Malformed",
        },
        &["TooLong", "Duplicate", "Malformed"],
    )
}

pub(crate) fn encoding_cases() -> Vec<Case<MalformedEncoding>> {
    vec![case("MalformedEncoding", MalformedEncoding)]
}

pub(crate) fn digest_cases() -> Vec<Case<InvalidDigest>> {
    vec![case("InvalidDigest", InvalidDigest)]
}

pub(crate) fn time_cases() -> Vec<Case<TimeOutOfRange>> {
    vec![case("TimeOutOfRange", TimeOutOfRange)]
}

pub(crate) fn project_cases() -> Vec<Case<InvalidProjectId>> {
    vec![case("InvalidProjectId", InvalidProjectId)]
}

pub(crate) fn conflicting_acl_cases() -> Vec<Case<ConflictingAcl>> {
    vec![case("ConflictingAcl", ConflictingAcl)]
}

pub(crate) fn glob_cases() -> Vec<Case<GlobError>> {
    every_variant(
        vec![
            mapping_case("TooLong", GlobError::TooLong),
            mapping_case("InvalidSyntax", GlobError::InvalidSyntax),
            mapping_case("UnclosedClass", GlobError::UnclosedClass(3)),
            mapping_case("UnclosedBrace", GlobError::UnclosedBrace(5)),
        ],
        |error| match error {
            GlobError::TooLong => "TooLong",
            GlobError::InvalidSyntax => "InvalidSyntax",
            GlobError::UnclosedClass(_) => "UnclosedClass",
            GlobError::UnclosedBrace(_) => "UnclosedBrace",
        },
        &["TooLong", "InvalidSyntax", "UnclosedClass", "UnclosedBrace"],
    )
}

pub(crate) fn url_cases() -> Vec<Case<UrlError>> {
    every_variant(
        vec![
            case("InvalidBaseUrl", UrlError::InvalidBaseUrl),
            case("UnsupportedBaseUrl", UrlError::UnsupportedBaseUrl),
            case("MissingAuthority", UrlError::MissingAuthority),
            case("MissingScheme", UrlError::MissingScheme),
            case("InvalidBasePort", UrlError::InvalidBasePort),
            case("OriginHasPath", UrlError::OriginHasPath),
            case("OriginPortZero", UrlError::OriginPortZero),
            case("InvalidOriginIpv6", UrlError::InvalidOriginIpv6),
            case("AmbiguousNumericHost", UrlError::AmbiguousNumericHost),
            case("InvalidOriginAuthority", UrlError::InvalidOriginAuthority),
            case(
                "InvalidDnsName",
                UrlError::InvalidDnsName(crate::config::InvalidDnsName),
            ),
            case(
                "InvalidBucket",
                UrlError::InvalidBucket(crate::bucket::name::InvalidBucketName),
            ),
            case(
                "InvalidObject",
                UrlError::InvalidObject("object name is empty".into()),
            ),
            case("ReservedNamespace", UrlError::ReservedNamespace),
            case("VirtualHostPathPrefix", UrlError::VirtualHostPathPrefix),
            case("VirtualHostBucketNotDns", UrlError::VirtualHostBucketNotDns),
            case("InvalidVirtualHostPort", UrlError::InvalidVirtualHostPort),
        ],
        |error| match error {
            UrlError::InvalidBaseUrl => "InvalidBaseUrl",
            UrlError::UnsupportedBaseUrl => "UnsupportedBaseUrl",
            UrlError::MissingAuthority => "MissingAuthority",
            UrlError::MissingScheme => "MissingScheme",
            UrlError::InvalidBasePort => "InvalidBasePort",
            UrlError::OriginHasPath => "OriginHasPath",
            UrlError::OriginPortZero => "OriginPortZero",
            UrlError::InvalidOriginIpv6 => "InvalidOriginIpv6",
            UrlError::AmbiguousNumericHost => "AmbiguousNumericHost",
            UrlError::InvalidOriginAuthority => "InvalidOriginAuthority",
            UrlError::InvalidDnsName(_) => "InvalidDnsName",
            UrlError::InvalidBucket(_) => "InvalidBucket",
            UrlError::InvalidObject(_) => "InvalidObject",
            UrlError::ReservedNamespace => "ReservedNamespace",
            UrlError::VirtualHostPathPrefix => "VirtualHostPathPrefix",
            UrlError::VirtualHostBucketNotDns => "VirtualHostBucketNotDns",
            UrlError::InvalidVirtualHostPort => "InvalidVirtualHostPort",
        },
        &[
            "InvalidBaseUrl",
            "UnsupportedBaseUrl",
            "MissingAuthority",
            "MissingScheme",
            "InvalidBasePort",
            "OriginHasPath",
            "OriginPortZero",
            "InvalidOriginIpv6",
            "AmbiguousNumericHost",
            "InvalidOriginAuthority",
            "InvalidDnsName",
            "InvalidBucket",
            "InvalidObject",
            "ReservedNamespace",
            "VirtualHostPathPrefix",
            "VirtualHostBucketNotDns",
            "InvalidVirtualHostPort",
        ],
    )
}

pub(crate) fn list_query_cases() -> Vec<Case<ListQueryError>> {
    every_variant(
        vec![
            mapping_case(
                "SoftDeletedWithVersions",
                ListQueryError::SoftDeletedWithVersions,
            ),
            mapping_case(
                "FoldersAsPrefixesNeedSlash",
                ListQueryError::FoldersAsPrefixesNeedSlash,
            ),
            mapping_case(
                "GlobNeedsSlashDelimiter",
                ListQueryError::GlobNeedsSlashDelimiter,
            ),
        ],
        |error| match error {
            ListQueryError::SoftDeletedWithVersions => "SoftDeletedWithVersions",
            ListQueryError::FoldersAsPrefixesNeedSlash => "FoldersAsPrefixesNeedSlash",
            ListQueryError::GlobNeedsSlashDelimiter => "GlobNeedsSlashDelimiter",
        },
        &[
            "SoftDeletedWithVersions",
            "FoldersAsPrefixesNeedSlash",
            "GlobNeedsSlashDelimiter",
        ],
    )
}

pub(crate) fn untestable_permission_cases() -> Vec<Case<UntestablePermission>> {
    every_variant(
        vec![
            case(
                "Unsupported",
                UntestablePermission::Unsupported("storage.managedFolders.get".into()),
            ),
            case(
                "Unknown",
                UntestablePermission::Unknown("storage.objects.fly".into()),
            ),
            case("ProjectScoped", UntestablePermission::ProjectScoped),
        ],
        |error| match error {
            UntestablePermission::Unsupported(_) => "Unsupported",
            UntestablePermission::Unknown(_) => "Unknown",
            UntestablePermission::ProjectScoped => "ProjectScoped",
        },
        &["Unsupported", "Unknown", "ProjectScoped"],
    )
}

pub(crate) fn policy_input_cases() -> Vec<Case<PolicyInputError>> {
    every_variant(
        vec![
            case(
                "InvalidInput",
                PolicyInputError::InvalidInput("Policy must be an object".into()),
            ),
            case(
                "UnsupportedInput",
                PolicyInputError::UnsupportedInput("role roles/owner".into()),
            ),
        ],
        |error| match error {
            PolicyInputError::InvalidInput(_) => "InvalidInput",
            PolicyInputError::UnsupportedInput(_) => "UnsupportedInput",
        },
        &["InvalidInput", "UnsupportedInput"],
    )
}

pub(crate) fn guidance_failures() -> Vec<(ObjectError, &'static str)> {
    use super::{object_unavailable, unavailable};
    use BackendFailure::*;
    let cases = [
        (
            CatalogClosed,
            "Object storage is unavailable: catalog is closed; wait for the server to restart before retrying",
        ),
        (
            UploadWaitTimeout,
            "Object storage is unavailable: upload catalog wait timed out; retry with generation preconditions or increase catalog_wait_timeout_ms (for example, 300000; 0 disables it); a completed upload may still commit",
        ),
        (
            RetainedSessionLimit,
            "Object storage is unavailable: retained upload session limit reached; raise max_resumable_sessions (0 is unlimited), wait for expired sessions to be reclaimed or use single-request media uploads",
        ),
        (
            RewriteSessionLimit,
            "Object storage is unavailable: rewrite session limit reached; raise max_rewrite_sessions (0 is unlimited) or wait for unfinished rewrites to expire",
        ),
        (
            FinalizationTimeout,
            "Object storage is unavailable: upload finalization timed out; read object state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)",
        ),
        (
            CopyDeadline,
            "Object storage is unavailable: copy request deadline exceeded; read destination state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)",
        ),
        (
            SessionBusy,
            "Object storage is unavailable: upload session is busy; wait for the current request to finish before sending another request for this session, or increase catalog_wait_timeout_ms (for example, 60000)",
        ),
        (
            MissingCompletion,
            "Object storage is unavailable: internal storage failure (private backend details withheld)",
        ),
    ];
    let mut failures: Vec<_> = cases
        .into_iter()
        .flat_map(|(cause, message)| {
            [
                (object_unavailable(cause), message),
                (ObjectError::from(unavailable(cause)), message),
            ]
        })
        .collect();
    for (failure, guidance) in crate::storage::catalog::test_fixtures::guidance_source_failures() {
        failures.push((
            failure,
            cases.iter().find(|(kind, _)| *kind == guidance).unwrap().1,
        ));
    }
    for error in [
        crate::server::copy::CheckpointError::ObserverDropped,
        crate::server::copy::CheckpointError::Abandoned,
    ] {
        let message = "Object storage is unavailable: internal storage failure (private backend details withheld)";
        failures.push((ObjectError::backend(error), message));
        failures.push((
            ObjectError::from(unavailable(ObjectError::backend(error))),
            message,
        ));
    }
    failures
}
