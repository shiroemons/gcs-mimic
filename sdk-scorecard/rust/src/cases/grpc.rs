//! gRPC probes through the SDK's public bidi read API. `Storage` serves one endpoint per
//! client, so the probe sets up over the HTTP client and reads through `Ctx::grpc_storage`.

use crate::harness::{Ctx, Result, check, drain, path};
use google_cloud_storage::model_ext::ReadRange;

pub(super) async fn multi_range_read(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.upload(&bucket, "mrd", b"0123456789").await?;
    let descriptor = ctx
        .grpc_storage()?
        .open_object(path(&bucket), "mrd")
        .send()
        .await?;
    // Both ranges are requested before either is drained, so they share one bidi session.
    let head = descriptor.read_range(ReadRange::segment(0, 3)).await;
    let tail = descriptor.read_range(ReadRange::segment(6, 4)).await;
    let head = drain(head).await?;
    let tail = drain(tail).await?;
    check(head == b"012", format!("range (0, 3) returned {head:?}"))?;
    check(tail == b"6789", format!("range (6, 4) returned {tail:?}"))
}
