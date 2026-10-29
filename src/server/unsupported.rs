use hyper::{Method, Uri};

// Storage v1 Discovery revision 20261020; keep explicit methods so object names remain opaque.
const ROUTES: &[(&str, &str)] = &[
    ("POST", "b/{bucket}/restore"),
    ("POST", "b/{bucket}/relocate"),
    ("GET", "b/{bucket}/storageLayout"),
    ("POST", "b/{bucket}/o"),
    ("POST", "b/{bucket}/o/bulkRestore"),
    ("GET|PUT", "b/{bucket}/o/{object}/iam"),
    ("GET", "b/{bucket}/o/{object}/iam/testPermissions"),
    ("GET", "b/{bucket}/o/{object}/viewFullContext"),
    ("GET|POST", "b/{bucket}/folders"),
    ("GET|DELETE", "b/{bucket}/folders/{folder}"),
    ("POST", "b/{bucket}/folders/{folder}/deleteRecursive"),
    (
        "POST",
        "b/{bucket}/folders/{sourceFolder}/renameTo/folders/{destinationFolder}",
    ),
    ("GET|POST", "b/{bucket}/managedFolders"),
    (
        "GET|DELETE|PATCH",
        "b/{bucket}/managedFolders/{managedFolder}",
    ),
    ("GET|PUT", "b/{bucket}/managedFolders/{managedFolder}/iam"),
    (
        "GET",
        "b/{bucket}/managedFolders/{managedFolder}/iam/testPermissions",
    ),
    ("GET", "b/{bucket}/operations"),
    ("GET", "b/{bucket}/operations/{operationId}"),
    ("POST", "b/{bucket}/operations/{operationId}/cancel"),
    (
        "POST",
        "b/{bucket}/operations/{operationId}/advanceRelocateBucket",
    ),
    ("GET|POST", "b/{bucket}/anywhereCaches"),
    ("GET|PATCH", "b/{bucket}/anywhereCaches/{anywhereCacheId}"),
    (
        "POST",
        "b/{bucket}/anywhereCaches/{anywhereCacheId}/disable",
    ),
    ("POST", "b/{bucket}/anywhereCaches/{anywhereCacheId}/pause"),
    ("POST", "b/{bucket}/anywhereCaches/{anywhereCacheId}/resume"),
    ("GET|POST", "b/{bucket}/rapidCaches"),
    ("GET|PATCH", "b/{bucket}/rapidCaches/{rapidCacheId}"),
    ("POST", "b/{bucket}/rapidCaches/{rapidCacheId}/disable"),
];

pub(super) fn route(method: &Method, uri: &Uri) -> Option<&'static str> {
    let path = uri.path().strip_prefix("/storage/v1/")?;
    ROUTES.iter().find_map(|(methods, template)| {
        if !methods.split('|').any(|allowed| allowed == method.as_str()) {
            return None;
        }
        let mut segments = path.split('/');
        let matches = template.split('/').all(|expected| {
            segments.next().is_some_and(|actual| {
                !actual.is_empty() && (expected.starts_with('{') || expected == actual)
            })
        });
        (matches && segments.next().is_none()).then_some(*template)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_discovery_routes_are_explicit_and_do_not_shadow_objects() {
        let inventory: serde_json::Value = serde_json::from_str(include_str!(
            "../../compatibility/upstream/json-methods.json"
        ))
        .unwrap();
        let implemented = [
            "storage.bucketAccessControls.delete",
            "storage.bucketAccessControls.get",
            "storage.bucketAccessControls.insert",
            "storage.bucketAccessControls.list",
            "storage.bucketAccessControls.patch",
            "storage.bucketAccessControls.update",
            "storage.defaultObjectAccessControls.delete",
            "storage.defaultObjectAccessControls.get",
            "storage.defaultObjectAccessControls.insert",
            "storage.defaultObjectAccessControls.list",
            "storage.defaultObjectAccessControls.patch",
            "storage.defaultObjectAccessControls.update",
            "storage.objectAccessControls.delete",
            "storage.objectAccessControls.get",
            "storage.objectAccessControls.insert",
            "storage.objectAccessControls.list",
            "storage.objectAccessControls.patch",
            "storage.objectAccessControls.update",
            "storage.buckets.delete",
            "storage.buckets.get",
            "storage.buckets.getIamPolicy",
            "storage.buckets.insert",
            "storage.buckets.list",
            "storage.buckets.lockRetentionPolicy",
            "storage.buckets.patch",
            "storage.buckets.setIamPolicy",
            "storage.buckets.testIamPermissions",
            "storage.buckets.update",
            "storage.notifications.delete",
            "storage.notifications.get",
            "storage.notifications.insert",
            "storage.notifications.list",
            "storage.objects.compose",
            "storage.objects.copy",
            "storage.objects.delete",
            "storage.objects.get",
            "storage.objects.list",
            "storage.objects.move",
            "storage.objects.patch",
            "storage.objects.rewrite",
            "storage.objects.restore",
            "storage.objects.update",
            "storage.projects.hmacKeys.create",
            "storage.projects.hmacKeys.delete",
            "storage.projects.hmacKeys.get",
            "storage.projects.hmacKeys.list",
            "storage.projects.hmacKeys.update",
            "storage.projects.serviceAccount.get",
        ];
        for operation in inventory["methods"].as_array().unwrap() {
            let path = operation["path"].as_str().unwrap();
            let uri: Uri = format!("/storage/v1/{path}")
                .replace('{', "%7B")
                .replace('}', "%7D")
                .parse()
                .unwrap();
            let method: Method = operation["http"].as_str().unwrap().parse().unwrap();
            assert_eq!(
                route(&method, &uri).is_none(),
                implemented.contains(&operation["id"].as_str().unwrap()),
                "{}",
                operation["id"]
            );
        }
        for path in [
            "/storage/v1/b/b/o/bulkRestore",
            "/storage/v1/b/b/o/a%2Facl",
            "/storage/v1/b/b/unknown",
            "/unknown",
        ] {
            assert!(
                route(&Method::GET, &path.parse().unwrap()).is_none(),
                "{path}"
            );
        }
        assert!(
            route(
                &Method::GET,
                &"/storage/v1/b/b/o/a%2Fb/iam".parse().unwrap()
            )
            .is_some()
        );
    }
}
