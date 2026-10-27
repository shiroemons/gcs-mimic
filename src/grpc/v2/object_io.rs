use super::{ResponseStream, Storage, pb, status::unsupported_for as unsupported};
use crate::auth::Caller;
use tonic::{Request, Response, Status, Streaming};

impl Storage {
    pub(super) async fn delete_object(
        &self,
        _request: Request<pb::DeleteObjectRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        Err(unsupported("DeleteObject", _caller))
    }
    pub(super) async fn restore_object(
        &self,
        _request: Request<pb::RestoreObjectRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        Err(unsupported("RestoreObject", _caller))
    }
    pub(super) async fn cancel_resumable_write(
        &self,
        _request: Request<pb::CancelResumableWriteRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::CancelResumableWriteResponse>, Status> {
        Err(unsupported("CancelResumableWrite", _caller))
    }
    pub(super) async fn get_object(
        &self,
        _request: Request<pb::GetObjectRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        Err(unsupported("GetObject", _caller))
    }
    pub(super) async fn read_object(
        &self,
        _request: Request<pb::ReadObjectRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::ReadObjectResponse>>, Status> {
        Err(unsupported("ReadObject", _caller))
    }
    pub(super) async fn write_object(
        &self,
        _request: Request<Streaming<pb::WriteObjectRequest>>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::WriteObjectResponse>, Status> {
        Err(unsupported("WriteObject", _caller))
    }
    pub(super) async fn bidi_write_object(
        &self,
        _request: Request<Streaming<pb::BidiWriteObjectRequest>>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::BidiWriteObjectResponse>>, Status> {
        Err(unsupported("BidiWriteObject", _caller))
    }
    pub(super) async fn list_objects(
        &self,
        _request: Request<pb::ListObjectsRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::ListObjectsResponse>, Status> {
        Err(unsupported("ListObjects", _caller))
    }
    pub(super) async fn start_resumable_write(
        &self,
        _request: Request<pb::StartResumableWriteRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::StartResumableWriteResponse>, Status> {
        Err(unsupported("StartResumableWrite", _caller))
    }
    pub(super) async fn query_write_status(
        &self,
        _request: Request<pb::QueryWriteStatusRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::QueryWriteStatusResponse>, Status> {
        Err(unsupported("QueryWriteStatus", _caller))
    }
}
