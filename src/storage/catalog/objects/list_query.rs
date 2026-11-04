use super::ObjectListQuery;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListQueryError {
    SoftDeletedWithVersions,
    FoldersAsPrefixesNeedSlash,
    GlobNeedsSlashDelimiter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FoldersAsPrefixes {
    Omitted,
    Requested,
}

impl ObjectListQuery {
    /// Preserve the JSON API's validation order; transports supply their own field names.
    pub(crate) fn validate(&self, folders: FoldersAsPrefixes) -> Result<(), ListQueryError> {
        if self.soft_deleted && self.versions {
            return Err(ListQueryError::SoftDeletedWithVersions);
        }
        if folders == FoldersAsPrefixes::Requested && self.delimiter != "/" {
            return Err(ListQueryError::FoldersAsPrefixesNeedSlash);
        }
        if self.match_glob.is_some() && !self.delimiter.is_empty() && self.delimiter != "/" {
            return Err(ListQueryError::GlobNeedsSlashDelimiter);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_combinations_keep_their_validation_order() {
        let mut query = ObjectListQuery {
            soft_deleted: true,
            versions: true,
            match_glob: Some(crate::match_glob::Glob::parse("**/*.txt").unwrap()),
            delimiter: "-".into(),
            ..Default::default()
        };
        assert_eq!(
            query.validate(FoldersAsPrefixes::Requested),
            Err(ListQueryError::SoftDeletedWithVersions)
        );
        query.versions = false;
        assert_eq!(
            query.validate(FoldersAsPrefixes::Requested),
            Err(ListQueryError::FoldersAsPrefixesNeedSlash)
        );
        assert_eq!(
            query.validate(FoldersAsPrefixes::Omitted),
            Err(ListQueryError::GlobNeedsSlashDelimiter)
        );
        query.delimiter = "/".into();
        assert_eq!(query.validate(FoldersAsPrefixes::Requested), Ok(()));
        query.delimiter.clear();
        assert_eq!(query.validate(FoldersAsPrefixes::Omitted), Ok(()));
    }
}
