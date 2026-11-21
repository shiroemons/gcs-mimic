use super::*;

impl Token {
    pub(crate) fn id(&self) -> OpaqueId {
        self.id
    }
}
impl Writeback {
    pub(crate) async fn page(&self, after: i64, limit: usize) -> Result<Vec<Pending>, Error> {
        let after = Ordinal::try_from(after).unwrap();
        let limit = PageLimit::try_from(limit).unwrap();
        self.serial_with(move |this| async move { this.page_inner(after, limit).await })
            .await
    }
}

#[test]
fn complete_phase_matches_static_active_row_predicates() {
    let connection = Connection::open_in_memory().unwrap();
    assert!(SCHEMA.contains("WHERE phase != 4"));
    assert!(SELECT_PENDING_PAGE.contains("phase!=4"));
    for phase in [
        Phase::Prepared,
        Phase::Armed,
        Phase::Protected,
        Phase::Durable,
        Phase::Complete,
    ] {
        assert_eq!(
            sql::scalar::<bool>(&connection, "SELECT ?1 != 4", [phase]).unwrap(),
            phase != Phase::Complete
        );
    }
}

/// Write-back operation IDs are stored as 32 lowercase hexadecimal digits; a row holding any
/// other ID is a corrupt catalog rather than an operation.
#[test]
fn write_back_rows_with_a_malformed_operation_id_are_corrupt() {
    let connection = Connection::open_in_memory().unwrap();
    let catalog = CatalogId::from_bytes([1; 16]);
    let root: RootId = "000102030405060708090a0b0c0d0e0f".parse().unwrap();
    let read = |id: &str| {
        sql::row(
            &connection,
            "SELECT ?1,1,'project','bucket','00000000000000000000000000000001','name','path',1,0,0,1,0,0,?2,NULL,0,?3,0",
            params![id, [0u8; 8], [0u8; 16]],
            |row| pending(row, catalog, root),
        )
    };
    let stored = "0123456789abcdef0123456789abcdef";
    assert_eq!(read(stored).unwrap().token.id().to_string(), stored);
    for id in [
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdefa",
        "../workspace-entry-name-length32",
    ] {
        let error = read(id).err().unwrap();
        assert!(
            matches!(error, rusqlite::Error::FromSqlConversionFailure(0, _, _)),
            "{id}"
        );
        assert!(
            matches!(Error::from(error), Error::Admin(AdminError::Corrupt(_))),
            "{id}"
        );
    }
}

/// Every failure of a write-back step answers whether it met a changed tree or catalog, whether
/// its target is out of reach and whether its operation is moot, as spelled out here rather
/// than taken from the classification under test. A step whose answer changed would cancel an
/// operation it should keep, or keep one it should cancel.
#[test]
fn a_step_failure_says_whether_it_met_a_change_lost_its_target_or_made_its_operation_moot() {
    let raced = |reason| Error::Raced(reason);
    let cases = [
        // (failure, changed, unreachable, superseded)
        (Error::Admin(AdminError::Precondition), true, true, true),
        (Error::Admin(AdminError::NotFound), false, false, true),
        (
            Error::Admin(AdminError::Io(io::ErrorKind::AlreadyExists.into())),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::Io(io::ErrorKind::NotFound.into())),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::Io(io::ErrorKind::TimedOut.into())),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::Busy {
                retry_after: std::time::Duration::ZERO,
            }),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::DiskFull(io::ErrorKind::StorageFull.into())),
            false,
            false,
            false,
        ),
        (Error::Admin(AdminError::Closed), false, false, false),
        (Error::Admin(AdminError::Hold), false, false, false),
        (
            Error::Admin(AdminError::SeedProjectMismatch),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::SeedMetadataBudget { mib: 1 }),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::SourceChanged { fields: Vec::new() }),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::Corrupt("corrupt".into())),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::Invalid("invalid".to_owned())),
            false,
            false,
            false,
        ),
        (Error::Admin(AdminError::Checksum), false, false, false),
        (Error::Admin(AdminError::TooLarge), false, false, false),
        (Error::Admin(AdminError::Forbidden), false, false, false),
        (
            Error::Admin(AdminError::BodyAborted("aborted".into())),
            false,
            false,
            false,
        ),
        (
            Error::Admin(AdminError::Backend("backend".into())),
            false,
            false,
            false,
        ),
        (
            Error::Root(RootError::Admin(AdminError::Precondition)),
            false,
            false,
            false,
        ),
        (
            Error::Root(RootError::UnopenableMarker),
            false,
            false,
            false,
        ),
        (Error::Root(RootError::InUse), false, false, false),
        (
            Error::Conflict(super::super::conflicts::Error::TargetChanged),
            false,
            false,
            false,
        ),
        (
            Error::Displaced(Box::new(Error::Raced(RaceReason::TargetChanged))),
            false,
            false,
            false,
        ),
        (Error::Capacity, false, false, false),
        (Error::RecoveryRequired, false, false, false),
        (Error::Protected, false, false, false),
        (Error::WorkspaceChanged, false, false, false),
        (raced(RaceReason::ParentChanged), true, true, true),
        (raced(RaceReason::Respelled), true, true, true),
        (raced(RaceReason::Unopenable), false, true, false),
        (raced(RaceReason::TargetChanged), true, true, true),
        (raced(RaceReason::DirectoryChanged), true, true, true),
        (Error::Obsolete(StaleReason::JournalFence), true, true, true),
        (Error::Obsolete(StaleReason::Bucket), true, true, true),
        (Error::Obsolete(StaleReason::Object), true, true, true),
        (Error::Obsolete(StaleReason::Tracked), true, true, true),
        (Error::Obsolete(StaleReason::Baseline), true, true, true),
        (Error::Obsolete(StaleReason::Revision), true, true, true),
        (Error::Obsolete(StaleReason::Completed), true, true, true),
        (Error::Obsolete(StaleReason::Root), true, true, true),
        (Error::Invariant("fixture"), true, true, true),
    ];
    for (error, changed, unreachable, superseded) in cases {
        assert_eq!(
            (error.changed(), error.unreachable(), error.superseded()),
            (changed, unreachable, superseded),
            "{error:?}"
        );
    }
}

#[test]
fn retained_limits_match_static_schema_bounds() {
    use super::super::conflicts::{MAX_BYTES, MAX_ENTRIES};
    assert!(SCHEMA.contains(&format!("max_bytes BETWEEN 1 AND {MAX_BYTES}")));
    assert!(SCHEMA.contains(&format!("max_entries BETWEEN 1 AND {MAX_ENTRIES}")));
    assert!(SCHEMA.contains(&format!("entries BETWEEN 0 AND {MAX_ENTRIES}")));
}
