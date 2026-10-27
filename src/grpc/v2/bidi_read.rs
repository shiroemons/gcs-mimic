use super::{ResponseStream, Storage, pb, status::unsupported_for as unsupported};
use crate::auth::Caller;
use tonic::{Request, Response, Status, Streaming};

impl Storage {
    pub(super) async fn bidi_read_object(
        &self,
        _request: Request<Streaming<pb::BidiReadObjectRequest>>,
        _caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::BidiReadObjectResponse>>, Status> {
        Err(unsupported("BidiReadObject", _caller))
    }
}
