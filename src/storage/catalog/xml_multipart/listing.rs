use super::*;
use crate::storage::catalog::sql;

#[derive(Clone)]
pub(crate) struct UploadQuery {
    pub(crate) bucket: String,
    pub(crate) prefix: String,
    pub(crate) delimiter: String,
    pub(crate) key_marker: String,
    pub(crate) upload_marker: Option<String>,
    pub(crate) limit: usize,
}

pub(crate) struct UploadPage {
    pub(crate) uploads: Vec<Upload>,
    pub(crate) prefixes: Vec<String>,
    pub(crate) next_key: String,
    pub(crate) next_upload: String,
    pub(crate) more: bool,
}

impl Catalog {
    pub(crate) async fn list_xml_uploads(
        &self,
        query: UploadQuery,
        access: BucketAccess,
    ) -> Result<UploadPage, Error> {
        let now = UnixMillis::try_from(self.0.policy.clock.now())?;
        self.run_read::<_, ObjectError>(move |connection| {
            let snapshot=sql::read_snapshot(connection)?;
            objects::authorized_bucket(&snapshot,&query.bucket,&access)?;
            let mut page=UploadPage {uploads:Vec::new(),prefixes:Vec::new(),next_key:query.key_marker.clone(),next_upload:query.upload_marker.clone().unwrap_or_default(),more:false};
            let mut after_name=query.key_marker.clone();
            let mut after_id=query.upload_marker.clone();
            let mut group=group_prefix(&query.key_marker,&query.prefix,&query.delimiter);
            loop {
                // A NULL `?4` makes `u.id>?4` never true, so without an upload ID the scan resumes
                // past every upload named `after_name`; names are never empty, so `''` starts it.
                let upload=sql::row(&snapshot, "SELECT u.id,u.bucket,u.name,u.incarnation,u.options,u.created,u.created_sub_milli_micros FROM xml_multipart_uploads u JOIN bucket_incarnations b ON b.bucket=u.bucket AND b.incarnation=u.incarnation WHERE u.bucket=?1 AND u.deadline>?2 AND (u.name>?3 OR (u.name=?3 AND u.id>?4)) ORDER BY u.name,u.id LIMIT 1", params![query.bucket,now,after_name,after_id],read_upload).optional()?;
                let Some(upload)=upload else {break};
                after_name.clone_from(&upload.target.name);after_id=Some(upload.target.id.clone());
                if !after_name.starts_with(&query.prefix) {continue;}
                if group.as_ref().is_some_and(|prefix|after_name.starts_with(prefix)) {continue;}
                let prefix=group_prefix(&after_name,&query.prefix,&query.delimiter);
                if page.uploads.len()+page.prefixes.len()>=query.limit.clamp(1,1000) {page.more=true;break;}
                if let Some(prefix)=prefix {
                    group=Some(prefix.clone());page.next_key.clone_from(&prefix);page.next_upload=String::new();page.prefixes.push(prefix);
                } else {
                    page.next_key.clone_from(&after_name);page.next_upload.clone_from(&upload.target.id);page.uploads.push(upload);
                }
            }
            Ok(page)
        }).await.map_err(Error::from)
    }
}

fn group_prefix(name: &str, prefix: &str, delimiter: &str) -> Option<String> {
    if delimiter.is_empty() {
        return None;
    }
    name.strip_prefix(prefix)?
        .find(delimiter)
        .and_then(|offset| name.get(..prefix.len() + offset + delimiter.len()))
        .map(str::to_owned)
}
