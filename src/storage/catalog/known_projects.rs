//! The projects the catalog knows by ID, for a request that names its project by number.

use super::*;
use crate::project::ProjectNumber;

/// Every project that owns a bucket, a soft-deleted bucket or an HMAC key, in byte order.
const KNOWN_PROJECTS: &str = "SELECT project FROM buckets UNION SELECT project FROM soft_deleted_buckets UNION SELECT project FROM hmac_keys ORDER BY 1";

impl Catalog {
    /// The first stored project, in byte order, whose number is `number`.
    pub(crate) async fn project_with_number(
        &self,
        number: ProjectNumber,
    ) -> Result<Option<String>, CatalogError> {
        self.run_read(move |connection| {
            let mut statement = connection.prepare_cached(KNOWN_PROJECTS)?;
            for project in statement.query_map([], |row| row.get::<_, String>(0))? {
                let project = project?;
                if ProjectNumber::of(&project) == number {
                    return Ok(Some(project));
                }
            }
            Ok(None)
        })
        .await
    }
}
