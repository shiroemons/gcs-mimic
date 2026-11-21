//! The proof that an operation's private entry holds a body whose removal loses nothing.

use super::*;

/// Proof that an operation's private entry holds a body whose removal loses nothing: the
/// operation's own API copy, which the API still holds, the tracked body that the API change
/// replaced, or a staged copy that no target took. Installation, cancellation and restart
/// recovery unlink an entry only through [`OwnedBody::unlink`], and only the constructors here
/// make one, so they are the only rules.
pub(in crate::storage::catalog::sync_state::writeback) struct OwnedBody(fsutil::FileIdentity);

impl OwnedBody {
    /// `file` when it holds exactly `item`'s API copy. Only a publication has one: a removal's
    /// row records the tracked body it removes, which only [`Self::tracked`] may prove.
    pub(in crate::storage::catalog::sync_state::writeback) fn api_copy(
        file: &File,
        item: &Pending,
    ) -> Result<Option<Self>, Error> {
        if item.kind != Kind::Publish {
            return Ok(None);
        }
        Ok(body::same_content(file, item.into())?.identity().map(Self))
    }

    /// `file` when it still holds the tracked body, by [`body::same_content`]: what
    /// installation's rename moved out of the target into the private entry, or what restart
    /// recovery finds there. Installation and recovery both prove it here, against the same record
    /// for an ordinary export, so recovery then removes no tracked body that installation would
    /// have kept. An export over a preserved conflict tracks the capture's record, while recovery
    /// tracks the one in `sync_objects`: after a crash right after the exchange, recovery keeps a
    /// captured body protected, and unlinks old tracked bytes written back before the exchange,
    /// which installation would have kept, as the tracked body an API change may replace.
    pub(in crate::storage::catalog::sync_state::writeback) fn tracked(
        file: &File,
        tracked: &SyncRecord,
    ) -> Result<Option<Self>, Error> {
        Ok(body::same_content(file, tracked.into())?
            .identity()
            .map(Self))
    }

    /// `file` as the private copy of `item` before it is armed. Only the copy step wrote it and
    /// no target has been replaced yet, so whatever it holds, even a partial copy, is the
    /// emulator's.
    pub(in crate::storage::catalog::sync_state::writeback) fn unarmed(
        file: &File,
        item: &Pending,
    ) -> Result<Option<Self>, Error> {
        if item.phase != Phase::Prepared || item.kind != Kind::Publish {
            return Ok(None);
        }
        Ok(Some(Self(fsutil::stat_identity(file)?)))
    }

    /// `file` as the copy that `item`, a publication, staged as `staged` and no target took:
    /// a cancelled export's own bytes. Anything else in the private entry, or a copy that
    /// changed since staging ([`confirm_copy`]), is `Protected`.
    pub(super) fn staged(
        file: &File,
        item: &Pending,
        staged: Option<fsutil::FileIdentity>,
    ) -> Result<Self, Error> {
        let (Kind::Publish, Some(staged)) = (item.kind, staged) else {
            return Err(Error::Protected);
        };
        Ok(Self(confirm_copy(file, staged, item, CopyCheck::Cancel)?))
    }

    /// Unlinks `item`'s private entry if its name still holds the body this proof names, and
    /// returns whether it did. A name that holds anything else by now keeps it, for the caller
    /// to protect.
    pub(in crate::storage::catalog::sync_state::writeback) fn unlink(
        self,
        workspace: &File,
        item: &Pending,
        #[cfg(test)] faults: &faults::Paths,
    ) -> Result<bool, Error> {
        let Some(current) = entry(workspace, item.token.id)? else {
            return Ok(false);
        };
        if fsutil::stat_identity(&current)? != self.0 {
            return Ok(false);
        }
        #[cfg(test)]
        faults.unlinking(item, &current)?;
        fsutil::unlink_file_at(workspace, item.token.id.to_hex().as_ref())?;
        Ok(true)
    }
}
