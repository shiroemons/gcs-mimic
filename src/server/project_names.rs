//! A request names its project by ID or by number, on every protocol (recorded 2026-11-17).
//! Buckets, grants and HMAC keys are kept under the ID, so a number is turned back into the ID
//! it was derived from before the request is authorized or reaches the catalog.

use super::State;
use crate::{bucket::CatalogError, project::ProjectNumber};

impl State {
    /// The ID of the project `named` names. A number names the project the emulator knows by
    /// that number: one the identity file names, or one that owns a bucket, a soft-deleted bucket
    /// or an HMAC key. Any other name is an ID already; so is a number no known project has,
    /// which names a project of its own, whose number is that same number.
    pub(crate) async fn project_id(&self, named: &str) -> Result<String, CatalogError> {
        let Some(number) = ProjectNumber::literal(named) else {
            return Ok(named.to_owned());
        };
        if let Some(id) = self
            .auth
            .project_ids()
            .find(|id| ProjectNumber::of(id) == number)
        {
            return Ok(id.to_owned());
        }
        Ok(self
            .catalog
            .project_with_number(number)
            .await?
            .unwrap_or_else(|| named.to_owned()))
    }
}
