//! Object listing probes through `StorageControl::list_objects` (gRPC), the Rust SDK's
//! only listing API.

use crate::harness::{Ctx, Result, check, sorted};

pub(super) async fn prefix_delimiter(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.seed(&bucket, &["a/1", "a/2", "b/1", "c"]).await?;
    let pages = ctx
        .list_pages(ctx.list_objects(&bucket)?.set_delimiter("/"), None)
        .await?;
    let names = sorted(
        pages
            .iter()
            .flat_map(|page| page.objects.iter().map(|o| o.name.clone())),
    );
    let prefixes = sorted(pages.into_iter().flat_map(|page| page.prefixes));
    check(names == ["c"], "names differ")?;
    check(prefixes == ["a/", "b/"], "prefixes differ")?;
    let names = ctx
        .list_names(ctx.list_objects(&bucket)?.set_prefix("a/"))
        .await?;
    check(names == ["a/1", "a/2"], "prefix listing")
}

pub(super) async fn paging(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let all = ["p1", "p2", "p3", "p4", "p5"];
    ctx.seed(&bucket, &all).await?;
    let pages = ctx
        .list_pages(ctx.list_objects(&bucket)?.set_page_size(2), Some(2))
        .await?;
    let count = pages.len();
    let found = sorted(
        pages
            .into_iter()
            .flat_map(|page| page.objects)
            .map(|o| o.name),
    );
    check(count >= 3 && found == all, format!("{found:?}"))
}

pub(super) async fn offsets(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.seed(&bucket, &["a", "b", "c", "d"]).await?;
    let request = ctx
        .list_objects(&bucket)?
        .set_lexicographic_start("b")
        .set_lexicographic_end("d");
    check(
        ctx.list_names(request).await? == ["b", "c"],
        "offset listing",
    )
}

pub(super) async fn match_glob(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.seed(&bucket, &["x/a.txt", "x/b.csv", "y/c.txt"])
        .await?;
    let request = ctx.list_objects(&bucket)?.set_match_glob("**/*.txt");
    check(
        ctx.list_names(request).await? == ["x/a.txt", "y/c.txt"],
        "glob listing",
    )
}
