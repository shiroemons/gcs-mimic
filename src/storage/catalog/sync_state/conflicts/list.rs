use super::*;

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Usage {
    pub reserved_bytes: u128,
    pub retained_bytes: u128,
    pub entries: u64,
    pub over_limit: bool,
}

/// Additional physical copies, including reservations while a protected source still exists.
pub(in crate::storage) fn usage(connection: &Connection, root: [u8; 16]) -> Result<Usage, Error> {
    let mut result = Usage::default();
    let mut statement = connection
        .prepare_cached("SELECT state,size FROM sync_conflicts WHERE root_id=?1 AND state!=2")?;
    let mut rows = statement.query([root])?;
    while let Some(row) = rows.next()? {
        let size = row.get::<_, i64>(1)? as u128;
        if row.get::<_, i64>(0)? == 0 {
            result.reserved_bytes += size
        } else {
            result.retained_bytes += size
        }
        result.entries += 1;
    }
    result.over_limit = result.reserved_bytes + result.retained_bytes > u128::from(MAX_BYTES)
        || result.entries > MAX_ENTRIES;
    Ok(result)
}

pub(super) fn total_usage(connection: &Connection, root: [u8; 16]) -> Result<Usage, Error> {
    let mut result = usage(connection, root)?;
    let workspace = connection
        .prepare_cached(
            "SELECT reserved,retained,entries FROM sync_writeback_roots WHERE root_id=?1",
        )?
        .query_row([root], |row| {
            Ok((
                row.get::<_, [u8; 16]>(0)?,
                row.get::<_, [u8; 16]>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .optional()?;
    if let Some((reserved, retained, entries)) = workspace {
        result.reserved_bytes += u128::from_be_bytes(reserved);
        result.retained_bytes += u128::from_be_bytes(retained);
        result.entries += entries as u64;
        result.entries = result
            .entries
            .checked_sub(transfer::captured_entries(connection, root)?)
            .ok_or(Error::TargetChanged)?;
    }
    result.over_limit = result.reserved_bytes + result.retained_bytes > u128::from(MAX_BYTES)
        || result.entries > MAX_ENTRIES;
    Ok(result)
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Conflict {
    pub conflict_id: String,
    pub revision: String,
    pub owner_project: String,
    pub bucket: String,
    pub bucket_incarnation: String,
    #[serde(rename = "object")]
    pub name: String,
    pub generation: Option<i64>,
    pub size: u64,
    pub hashes: Option<Hashes>,
    pub phase: &'static str,
    pub remedy: &'static str,
    #[serde(skip)]
    pub ordinal: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConflictPage {
    pub revision: String,
    pub items: Vec<super::Conflict>,
    pub usage: Usage,
    pub next_after: Option<i64>,
}

impl RootLease {
    pub(crate) async fn conflict_page(
        &self,
        revision: Option<String>,
        after: i64,
        limit: usize,
        authority: ControlAuthority,
    ) -> Result<ConflictPage, Error> {
        if after < 0 || !(1..=256).contains(&limit) || (after > 0 && revision.is_none()) {
            return Err(Error::InvalidResolution);
        }
        let root = self.status().root_id.as_bytes();
        self.conflict_db(move|connection| {
            let tx=connection.transaction()?;
            authority.authorize(&tx,root)?;
            tx.prepare_cached("INSERT INTO sync_conflict_roots(root_id,revision) VALUES(?1,?2) ON CONFLICT(root_id) DO NOTHING")?.execute(params![root,opaque_id()?])?;
            let current:String=tx.prepare_cached("SELECT revision FROM sync_conflict_roots WHERE root_id=?1")?.query_row([root],|row|row.get(0))?;
            if revision.is_some_and(|revision|revision!=current) {return Err(Error::RevisionMismatch)}
            let usage=total_usage(&tx,root)?;
            let mut statement=tx.prepare_cached("SELECT ordinal,id,revision,owner,bucket,incarnation,name,generation,size,retained,state FROM sync_conflicts WHERE root_id=?1 AND ordinal>?2 AND state!=2 ORDER BY ordinal LIMIT ?3")?;
            let mut rows=statement.query(params![root,after,(limit+1) as i64])?;
            let mut items=Vec::with_capacity(limit);
            let mut bytes=0;
            let mut more=false;
            while let Some(row)=rows.next()? {
                let retained:Option<Vec<u8>>=row.get(9)?;
                let hashes=retained.as_deref().map(Observation::decode).transpose()?.and_then(|observation|observation.hashes).map(|h|Hashes {md5:h.md5,crc32c:h.crc32c});
                let item=Conflict {ordinal:row.get(0)?,conflict_id:row.get(1)?,revision:row.get(2)?,owner_project:row.get(3)?,bucket:row.get(4)?,bucket_incarnation:row.get(5)?,name:row.get(6)?,generation:row.get(7)?,size:row.get::<_,i64>(8)? as u64,hashes,
                    phase:if row.get::<_,i64>(10)?==0 {"capturing"} else {"registered"},remedy:"Inspect the current generation, then restore or explicitly discard this revision."};
                let size=serde_json::to_vec(&item).map_err(AdminError::backend)?.len();
                if items.len()==limit || bytes+size>240_000 {more=true;break}
                bytes+=size;items.push(item);
            }
            drop(rows);drop(statement);
            let next_after=if more {items.last().map(|item|item.ordinal)} else {None};
            tx.commit()?;
            Ok(ConflictPage {revision:current,items,usage,next_after})
        }).await
    }
}
