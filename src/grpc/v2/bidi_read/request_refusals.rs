//! BidiReadObject requests refused before any data is sent. Malformed input answers
//! INVALID_ARGUMENT; redirects, nondefault read masks and conditions on a resumed handle, which
//! gcs-mimic does not model, answer UNIMPLEMENTED with ErrorInfo.

use super::*;
use crate::grpc::test_support::{assert_code, assert_unsupported, mask, rpc_status};
use crate::{auth::Auth, storage::Catalog, test_support::TestDir};
use std::sync::Arc;

const PATH: &str = "/google.storage.v2.Storage/BidiReadObject";

async fn setup() -> (TestDir, Arc<crate::server::State>) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(crate::storage::test_fixtures::bucket("bidi-refusals", "p"))
        .await
        .unwrap();
    crate::grpc::test_support::put_keyed(&catalog, "bidi-refusals", "object", b"abc", None).await;
    let state = Arc::new(crate::server::State::for_test(catalog, Auth::permissive()));
    (dir, state)
}

fn spec() -> pb::BidiReadObjectSpec {
    pb::BidiReadObjectSpec {
        bucket: "projects/_/buckets/bidi-refusals".into(),
        object: "object".into(),
        ..Default::default()
    }
}

fn opening(spec: pb::BidiReadObjectSpec) -> pb::BidiReadObjectRequest {
    pb::BidiReadObjectRequest {
        read_object_spec: Some(spec),
        read_ranges: vec![pb::ReadRange {
            read_id: 1,
            ..Default::default()
        }],
    }
}

/// Runs one stream over the wire, with an optional `x-goog-fieldmask` header.
async fn stream(
    state: &Arc<crate::server::State>,
    messages: &[pb::BidiReadObjectRequest],
    fieldmask: Option<&'static str>,
) -> (Status, Vec<pb::BidiReadObjectResponse>) {
    rpc_status(PATH, None, messages, async |mut request| {
        if let Some(value) = fieldmask {
            request.headers_mut().insert(
                "x-goog-fieldmask",
                hyper::header::HeaderValue::from_static(value),
            );
        }
        crate::grpc::v2::dispatch(
            crate::grpc::v2::service(
                state.clone(),
                1024 * 1024,
                crate::config::CompatProfile::Native,
            ),
            request,
        )
        .await
        .unwrap()
    })
    .await
}

#[track_caller]
fn assert_status(status: &Status, code: tonic::Code, message: &str) {
    assert_eq!(status.code(), code, "{status:?}");
    assert!(status.message().contains(message), "{message}: {status:?}");
}

/// The status read back from the wire carries one `google.rpc.ErrorInfo`.
#[track_caller]
fn assert_error_info(status: &Status) {
    use prost::Message;
    let rpc = crate::grpc::generated::google::rpc::Status::decode(status.details()).unwrap();
    let [detail] = rpc.details.as_slice() else {
        panic!("one detail expected: {rpc:?}");
    };
    assert_eq!(detail.type_url, "type.googleapis.com/google.rpc.ErrorInfo");
}

#[test]
#[expect(
    deprecated,
    reason = "the deprecated read_mask field is set to check that it is still refused"
)]
fn the_spec_refuses_redirects_nondefault_masks_and_conditions_on_a_handle() {
    let unsupported = "BidiReadObject routing_token or read_mask";
    let routed = pb::BidiReadObjectSpec {
        routing_token: Some("token".into()),
        ..spec()
    };
    assert_unsupported(validate_spec(&routed), unsupported);
    for paths in [&["name"][..], &["*", "name"][..]] {
        let masked = pb::BidiReadObjectSpec {
            read_mask: mask(paths),
            ..spec()
        };
        assert_unsupported(validate_spec(&masked), unsupported);
    }
    // An empty routing token, an absent or empty mask and `*` keep the default read.
    for read_mask in [None, mask(&[]), mask(&["*"])] {
        let default = pb::BidiReadObjectSpec {
            routing_token: Some(String::new()),
            read_mask,
            ..spec()
        };
        assert_eq!(validate_spec(&default).unwrap(), None);
    }
    let handle = Some(pb::BidiReadHandle {
        handle: vec![0; 32],
    });
    let conditions: [fn(&mut pb::BidiReadObjectSpec); 4] = [
        |spec| spec.if_generation_match = Some(1),
        |spec| spec.if_generation_not_match = Some(1),
        |spec| spec.if_metageneration_match = Some(1),
        |spec| spec.if_metageneration_not_match = Some(1),
    ];
    for condition in conditions {
        let mut resumed = pb::BidiReadObjectSpec {
            read_handle: handle.clone(),
            ..spec()
        };
        condition(&mut resumed);
        assert_unsupported(
            validate_spec(&resumed),
            "BidiReadObject preconditions with read_handle",
        );
    }
    let message = "Generation conditions must be nonnegative; use generation 0 for the live object";
    let negative = [
        pb::BidiReadObjectSpec {
            generation: -1,
            ..spec()
        },
        pb::BidiReadObjectSpec {
            if_metageneration_match: Some(-1),
            ..spec()
        },
    ];
    for spec in negative {
        let status = assert_code(validate_spec(&spec), tonic::Code::InvalidArgument);
        assert_eq!(status.message(), message);
    }
}

#[tokio::test]
async fn the_stream_refuses_a_missing_or_late_spec_and_a_field_mask_header() {
    let (_dir, state) = setup().await;
    let (status, responses) = stream(&state, &[], None).await;
    assert_status(
        &status,
        tonic::Code::InvalidArgument,
        "First message must contain read_object_spec",
    );
    assert!(responses.is_empty());
    let (status, responses) = stream(&state, &[pb::BidiReadObjectRequest::default()], None).await;
    assert_status(
        &status,
        tonic::Code::InvalidArgument,
        "First message requires read_object_spec",
    );
    assert!(responses.is_empty());

    let (status, responses) = stream(&state, &[opening(spec())], Some("name")).await;
    assert_error_info(&status);
    assert_unsupported(Err::<(), _>(status), "BidiReadObject x-goog-fieldmask");
    assert!(responses.is_empty());
    let (status, _) = stream(&state, &[opening(spec())], Some("*")).await;
    assert_eq!(status.code(), tonic::Code::Ok, "{status:?}");

    // A later message may only add ranges.
    let late = pb::BidiReadObjectRequest {
        read_object_spec: Some(spec()),
        read_ranges: Vec::new(),
    };
    let (status, _) = stream(&state, &[opening(spec()), late], None).await;
    assert_status(
        &status,
        tonic::Code::InvalidArgument,
        "read_object_spec is only allowed in the first message",
    );
    state.catalog.close().await.unwrap();
}
