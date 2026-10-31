use super::*;

pub(crate) struct UploadQuery {
    pub bucket: String,
    pub prefix: String,
    pub delimiter: String,
    pub key_marker: String,
    pub upload_marker: Option<String>,
    pub limit: usize,
}

pub(crate) struct UploadPage {
    pub uploads: Vec<Upload>,
    pub prefixes: Vec<String>,
    pub next_key: String,
    pub next_upload: String,
    pub more: bool,
}

impl Catalog {
    pub(crate) async fn list_xml_uploads(
        &self,
        query: UploadQuery,
        access: BucketAccess,
    ) -> Result<UploadPage, Error> {
        let now = unix_millis(self.0.clock.now())?;
        self.run_object(move |connection| {
            let transaction=connection.transaction().map_err(object_unavailable)?;
            objects::authorized_bucket(&transaction,&query.bucket,&access)?;
            let mut page=UploadPage {uploads:Vec::new(),prefixes:Vec::new(),next_key:query.key_marker.clone(),next_upload:query.upload_marker.clone().unwrap_or_default(),more:false};
            let mut after_name=query.key_marker.clone();
            let mut after_id=query.upload_marker.clone().unwrap_or_else(||if query.key_marker.is_empty(){String::new()}else{"~".into()});
            let mut group=group_prefix(&query.key_marker,&query.prefix,&query.delimiter);
            loop {
                let upload=transaction.prepare_cached("SELECT u.id,u.bucket,u.name,u.incarnation,u.options,u.created FROM xml_multipart_uploads u JOIN bucket_incarnations b ON b.bucket=u.bucket AND b.incarnation=u.incarnation WHERE u.bucket=?1 AND u.deadline>?2 AND (u.name>?3 OR (u.name=?3 AND u.id>?4)) ORDER BY u.name,u.id LIMIT 1")
                    .and_then(|mut s|s.query_row(params![query.bucket,now,after_name,after_id],read_upload)).optional().map_err(object_unavailable)?;
                let Some(upload)=upload else {break};
                after_name=upload.target.name.clone();after_id=upload.target.id.clone();
                if !after_name.starts_with(&query.prefix) {continue;}
                if group.as_ref().is_some_and(|prefix|after_name.starts_with(prefix)) {continue;}
                let prefix=group_prefix(&after_name,&query.prefix,&query.delimiter);
                if page.uploads.len()+page.prefixes.len()>=query.limit.clamp(1,1000) {page.more=true;break;}
                if let Some(prefix)=prefix {
                    group=Some(prefix.clone());page.next_key=prefix.clone();page.next_upload=String::new();page.prefixes.push(prefix);
                } else {
                    page.uploads.push(upload);page.next_key=after_name.clone();page.next_upload=after_id.clone();
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
        .map(|offset| name[..prefix.len() + offset + delimiter.len()].to_owned())
}
