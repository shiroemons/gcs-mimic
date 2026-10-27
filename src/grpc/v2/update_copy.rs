use super::{Storage, pb, status::unsupported_for as unsupported};
use crate::auth::Caller;
use tonic::{Request, Response, Status};

impl Storage {
    pub(super) async fn compose_object(
        &self,
        _request: Request<pb::ComposeObjectRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        Err(unsupported("ComposeObject", _caller))
    }
    pub(super) async fn update_object(
        &self,
        _request: Request<pb::UpdateObjectRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        Err(unsupported("UpdateObject", _caller))
    }
    pub(super) async fn rewrite_object(
        &self,
        _request: Request<pb::RewriteObjectRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::RewriteResponse>, Status> {
        Err(unsupported("RewriteObject", _caller))
    }
    pub(super) async fn move_object(
        &self,
        _request: Request<pb::MoveObjectRequest>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        Err(unsupported("MoveObject", _caller))
    }
}
