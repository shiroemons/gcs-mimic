use super::*;
use crate::config::CompatProfile;

/// A rewrite session stores the bound parameters as JSON, key order included.
#[test]
fn bound_parameters_keep_their_json_bytes() {
    let base = "/storage/v1/b/source-bucket/o/a/rewriteTo/b/destination-bucket/o/c";
    // Defaults the request left out follow its own parameters, in this order.
    for (query, bound) in [
        (
            "?sourceGeneration=5&ifGenerationMatch=007&ifSourceMetagenerationNotMatch=-0&fields=done&prettyPrint=False&projection=noAcl&destinationPredefinedAcl=private&maxBytesRewrittenPerCall=1048576&gcsmimicUnknown=1&key=k",
            r#"{"destinationPredefinedAcl":"private","fields":"done","ifGenerationMatch":"7","ifSourceMetagenerationNotMatch":"0","maxBytesRewrittenPerCall":"1048576","prettyPrint":"False","projection":"noAcl","sourceGeneration":"5","alt":"json"}"#,
        ),
        (
            "?maxBytesRewrittenPerCall=1048576",
            r#"{"maxBytesRewrittenPerCall":"1048576","projection":"noAcl","alt":"json","prettyPrint":"false"}"#,
        ),
    ] {
        let operation = crate::json::rewrite::route(
            &Method::POST,
            &format!("{base}{query}").parse().unwrap(),
            CompatProfile::Native,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::to_string(&bound_parameters(&operation.parameters)).unwrap(),
            bound
        );
    }
}
