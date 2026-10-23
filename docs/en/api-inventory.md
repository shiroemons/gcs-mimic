# Public API inventory

Research cutoff: **2026-10-21**. Core JSON Discovery revision **20261013**.
This inventory describes the upstream surface; implementation status is owned
by `compatibility/features.json`.

## Counting rules

The following counts describe separately tracked operation/transport units, not unique product features. XML metadata subresources are counted as variants. Shared RPC definitions are not promises that each service implements every method. REST and native RPC can address the same domain operation; both require conformance evidence. The ledger separately tracks behavior and project requirements.

| Surface | Tracked operations |
| --- | ---: |
| Core JSON API v1 | 87 |
| gRPC Storage v2 | 24 |
| FeatureConfigs v2 REST | 5 |
| XML API | 52 |
| Storage Control v2 | 39 |
| Storage Insights | 20 |
| Storage Batch Operations | 13 |
| Storage Intelligence | 11 |
| Cloud Storage MCP | 9 |
| Shared long-running RPC | 5 |
| Storage Insights RPC | 14 |
| Storage Batch Operations RPC | 7 |
| Shared location RPC | 4 |
| Storage Control v2 REST binding | 7 |
| Storage Control REST | 1 |
| Total tracked operation rows | 298 |

The live JSON snapshot has 38 schema definitions. Per-property and per-parameter status inventories are separate files. Method presence is not evidence of full field, permission or error compatibility.

`compatibility/upstream/json-schemas.json` also supplies runtime validation for
JSON `fields` selectors over Bucket, Buckets, Object, Objects and Policy responses.
Refreshing that inventory changes the accepted selectors and requires the
renderer/schema regression test in `src/json/fields.rs`. Resumable uploads
validate against it at initiation; later chunks use the persisted selector
without schema revalidation.

Storage Control has 39 direct RPCs in the current Google-owned proto and generated Go reference, which is broader than the shorter HTML RPC page. Insights has 14 and Batch Operations7 direct RPCs. The source manifest preserves digests and service registration information.

Open specification questions are retained in `compatibility/gaps.json`. Full-context v1/v2 routes are distinct advertised API versions, not a source contradiction; each remains pending implementation and verification.

## Core JSON API v1

Bucket IAM get/set use the [scoped bucket policy contract](slices/bucket-iam.md),
checked against the same live Discovery revision on 2026-10-22. The supported
subset includes persistent unconditional predefined storage roles, local user
and service-account members, convenience values, effective project/bucket grants
and atomic etag updates. `testIamPermissions`, public principals, conditions,
custom/basic roles and ACLs remain outside that scope. Get/set require
`storage.buckets.getIamPolicy` / `storage.buckets.setIamPolicy`; bucket-level
grants never authorize project-scoped bucket insert/list.

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| JSON-ANYWHERECACHES-001 | storage.anywhereCaches.disable | POST /storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId}/disable |
| JSON-ANYWHERECACHES-002 | storage.anywhereCaches.get | GET /storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId} |
| JSON-ANYWHERECACHES-003 | storage.anywhereCaches.insert | POST /storage/v1/b/{bucket}/anywhereCaches |
| JSON-ANYWHERECACHES-004 | storage.anywhereCaches.list | GET /storage/v1/b/{bucket}/anywhereCaches |
| JSON-ANYWHERECACHES-005 | storage.anywhereCaches.pause | POST /storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId}/pause |
| JSON-ANYWHERECACHES-006 | storage.anywhereCaches.resume | POST /storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId}/resume |
| JSON-ANYWHERECACHES-007 | storage.anywhereCaches.update | PATCH /storage/v1/b/{bucket}/anywhereCaches/{anywhereCacheId} |
| JSON-BUCKETACCESSCONTROLS-001 | storage.bucketAccessControls.delete | DELETE /storage/v1/b/{bucket}/acl/{entity} |
| JSON-BUCKETACCESSCONTROLS-002 | storage.bucketAccessControls.get | GET /storage/v1/b/{bucket}/acl/{entity} |
| JSON-BUCKETACCESSCONTROLS-003 | storage.bucketAccessControls.insert | POST /storage/v1/b/{bucket}/acl |
| JSON-BUCKETACCESSCONTROLS-004 | storage.bucketAccessControls.list | GET /storage/v1/b/{bucket}/acl |
| JSON-BUCKETACCESSCONTROLS-005 | storage.bucketAccessControls.patch | PATCH /storage/v1/b/{bucket}/acl/{entity} |
| JSON-BUCKETACCESSCONTROLS-006 | storage.bucketAccessControls.update | PUT /storage/v1/b/{bucket}/acl/{entity} |
| JSON-BUCKETS-001 | storage.buckets.delete | DELETE /storage/v1/b/{bucket} |
| JSON-BUCKETS-002 | storage.buckets.get | GET /storage/v1/b/{bucket} |
| JSON-BUCKETS-003 | storage.buckets.getIamPolicy | GET /storage/v1/b/{bucket}/iam |
| JSON-BUCKETS-004 | storage.buckets.getStorageLayout | GET /storage/v1/b/{bucket}/storageLayout |
| JSON-BUCKETS-005 | storage.buckets.insert | POST /storage/v1/b |
| JSON-BUCKETS-006 | storage.buckets.list | GET /storage/v1/b |
| JSON-BUCKETS-007 | storage.buckets.lockRetentionPolicy | POST /storage/v1/b/{bucket}/lockRetentionPolicy |
| JSON-BUCKETS-008 | storage.buckets.patch | PATCH /storage/v1/b/{bucket} |
| JSON-BUCKETS-009 | storage.buckets.relocate | POST /storage/v1/b/{bucket}/relocate |
| JSON-BUCKETS-010 | storage.buckets.restore | POST /storage/v1/b/{bucket}/restore |
| JSON-BUCKETS-011 | storage.buckets.setIamPolicy | PUT /storage/v1/b/{bucket}/iam |
| JSON-BUCKETS-012 | storage.buckets.testIamPermissions | GET /storage/v1/b/{bucket}/iam/testPermissions |
| JSON-BUCKETS-013 | storage.buckets.update | PUT /storage/v1/b/{bucket} |
| JSON-DEFAULTOBJECTACCESSCONTROLS-001 | storage.defaultObjectAccessControls.delete | DELETE /storage/v1/b/{bucket}/defaultObjectAcl/{entity} |
| JSON-DEFAULTOBJECTACCESSCONTROLS-002 | storage.defaultObjectAccessControls.get | GET /storage/v1/b/{bucket}/defaultObjectAcl/{entity} |
| JSON-DEFAULTOBJECTACCESSCONTROLS-003 | storage.defaultObjectAccessControls.insert | POST /storage/v1/b/{bucket}/defaultObjectAcl |
| JSON-DEFAULTOBJECTACCESSCONTROLS-004 | storage.defaultObjectAccessControls.list | GET /storage/v1/b/{bucket}/defaultObjectAcl |
| JSON-DEFAULTOBJECTACCESSCONTROLS-005 | storage.defaultObjectAccessControls.patch | PATCH /storage/v1/b/{bucket}/defaultObjectAcl/{entity} |
| JSON-DEFAULTOBJECTACCESSCONTROLS-006 | storage.defaultObjectAccessControls.update | PUT /storage/v1/b/{bucket}/defaultObjectAcl/{entity} |
| JSON-FOLDERS-001 | storage.folders.delete | DELETE /storage/v1/b/{bucket}/folders/{folder} |
| JSON-FOLDERS-002 | storage.folders.deleteRecursive | POST /storage/v1/b/{bucket}/folders/{folder}/deleteRecursive |
| JSON-FOLDERS-003 | storage.folders.get | GET /storage/v1/b/{bucket}/folders/{folder} |
| JSON-FOLDERS-004 | storage.folders.insert | POST /storage/v1/b/{bucket}/folders |
| JSON-FOLDERS-005 | storage.folders.list | GET /storage/v1/b/{bucket}/folders |
| JSON-FOLDERS-006 | storage.folders.rename | POST /storage/v1/b/{bucket}/folders/{sourceFolder}/renameTo/folders/{destinationFolder} |
| JSON-MANAGEDFOLDERS-001 | storage.managedFolders.delete | DELETE /storage/v1/b/{bucket}/managedFolders/{managedFolder} |
| JSON-MANAGEDFOLDERS-002 | storage.managedFolders.get | GET /storage/v1/b/{bucket}/managedFolders/{managedFolder} |
| JSON-MANAGEDFOLDERS-003 | storage.managedFolders.getIamPolicy | GET /storage/v1/b/{bucket}/managedFolders/{managedFolder}/iam |
| JSON-MANAGEDFOLDERS-004 | storage.managedFolders.insert | POST /storage/v1/b/{bucket}/managedFolders |
| JSON-MANAGEDFOLDERS-005 | storage.managedFolders.list | GET /storage/v1/b/{bucket}/managedFolders |
| JSON-MANAGEDFOLDERS-006 | storage.managedFolders.setIamPolicy | PUT /storage/v1/b/{bucket}/managedFolders/{managedFolder}/iam |
| JSON-MANAGEDFOLDERS-007 | storage.managedFolders.testIamPermissions | GET /storage/v1/b/{bucket}/managedFolders/{managedFolder}/iam/testPermissions |
| JSON-MANAGEDFOLDERS-008 | storage.managedFolders.update | PATCH /storage/v1/b/{bucket}/managedFolders/{managedFolder} |
| JSON-NOTIFICATIONS-001 | storage.notifications.delete | DELETE /storage/v1/b/{bucket}/notificationConfigs/{notification} |
| JSON-NOTIFICATIONS-002 | storage.notifications.get | GET /storage/v1/b/{bucket}/notificationConfigs/{notification} |
| JSON-NOTIFICATIONS-003 | storage.notifications.insert | POST /storage/v1/b/{bucket}/notificationConfigs |
| JSON-NOTIFICATIONS-004 | storage.notifications.list | GET /storage/v1/b/{bucket}/notificationConfigs |
| JSON-OBJECTACCESSCONTROLS-001 | storage.objectAccessControls.delete | DELETE /storage/v1/b/{bucket}/o/{object}/acl/{entity} |
| JSON-OBJECTACCESSCONTROLS-002 | storage.objectAccessControls.get | GET /storage/v1/b/{bucket}/o/{object}/acl/{entity} |
| JSON-OBJECTACCESSCONTROLS-003 | storage.objectAccessControls.insert | POST /storage/v1/b/{bucket}/o/{object}/acl |
| JSON-OBJECTACCESSCONTROLS-004 | storage.objectAccessControls.list | GET /storage/v1/b/{bucket}/o/{object}/acl |
| JSON-OBJECTACCESSCONTROLS-005 | storage.objectAccessControls.patch | PATCH /storage/v1/b/{bucket}/o/{object}/acl/{entity} |
| JSON-OBJECTACCESSCONTROLS-006 | storage.objectAccessControls.update | PUT /storage/v1/b/{bucket}/o/{object}/acl/{entity} |
| JSON-OBJECTS-001 | storage.objects.bulkRestore | POST /storage/v1/b/{bucket}/o/bulkRestore |
| JSON-OBJECTS-002 | storage.objects.compose | POST /storage/v1/b/{destinationBucket}/o/{destinationObject}/compose |
| JSON-OBJECTS-003 | storage.objects.copy | POST /storage/v1/b/{sourceBucket}/o/{sourceObject}/copyTo/b/{destinationBucket}/o/{destinationObject} |
| JSON-OBJECTS-004 | storage.objects.delete | DELETE /storage/v1/b/{bucket}/o/{object} |
| JSON-OBJECTS-005 | storage.objects.get | GET /storage/v1/b/{bucket}/o/{object} |
| JSON-OBJECTS-006 | storage.objects.getIamPolicy | GET /storage/v1/b/{bucket}/o/{object}/iam |
| JSON-OBJECTS-007 | storage.objects.insert | POST /upload/storage/v1/b/{bucket}/o with uploadType=media, multipart or resumable; Discovery also advertises /resumable/upload/storage/v1/b/{bucket}/o for resumable initiation. The generic method template is not a media-upload route. |
| JSON-OBJECTS-008 | storage.objects.list | GET /storage/v1/b/{bucket}/o |
| JSON-OBJECTS-009 | storage.objects.move | POST /storage/v1/b/{bucket}/o/{sourceObject}/moveTo/o/{destinationObject} |
| JSON-OBJECTS-010 | storage.objects.patch | PATCH /storage/v1/b/{bucket}/o/{object} |
| JSON-OBJECTS-011 | storage.objects.restore | POST /storage/v1/b/{bucket}/o/{object}/restore |
| JSON-OBJECTS-012 | storage.objects.rewrite | POST /storage/v1/b/{sourceBucket}/o/{sourceObject}/rewriteTo/b/{destinationBucket}/o/{destinationObject} |
| JSON-OBJECTS-013 | storage.objects.setIamPolicy | PUT /storage/v1/b/{bucket}/o/{object}/iam |
| JSON-OBJECTS-014 | storage.objects.testIamPermissions | GET /storage/v1/b/{bucket}/o/{object}/iam/testPermissions |
| JSON-OBJECTS-015 | storage.objects.update | PUT /storage/v1/b/{bucket}/o/{object} |
| JSON-OBJECTS-016 | storage.objects.viewFullContext | GET /storage/v1/b/{bucket}/o/{object}/viewFullContext |
| JSON-OPERATIONS-001 | storage.buckets.operations.advanceRelocateBucket | POST /storage/v1/b/{bucket}/operations/{operationId}/advanceRelocateBucket |
| JSON-OPERATIONS-002 | storage.buckets.operations.cancel | POST /storage/v1/b/{bucket}/operations/{operationId}/cancel |
| JSON-OPERATIONS-003 | storage.buckets.operations.get | GET /storage/v1/b/{bucket}/operations/{operationId} |
| JSON-OPERATIONS-004 | storage.buckets.operations.list | GET /storage/v1/b/{bucket}/operations |
| JSON-PROJECTS-HMACKEYS-001 | storage.projects.hmacKeys.create | POST /storage/v1/projects/{projectId}/hmacKeys |
| JSON-PROJECTS-HMACKEYS-002 | storage.projects.hmacKeys.delete | DELETE /storage/v1/projects/{projectId}/hmacKeys/{accessId} |
| JSON-PROJECTS-HMACKEYS-003 | storage.projects.hmacKeys.get | GET /storage/v1/projects/{projectId}/hmacKeys/{accessId} |
| JSON-PROJECTS-HMACKEYS-004 | storage.projects.hmacKeys.list | GET /storage/v1/projects/{projectId}/hmacKeys |
| JSON-PROJECTS-HMACKEYS-005 | storage.projects.hmacKeys.update | PUT /storage/v1/projects/{projectId}/hmacKeys/{accessId} |
| JSON-PROJECTS-SERVICEACCOUNT-001 | storage.projects.serviceAccount.get | GET /storage/v1/projects/{projectId}/serviceAccount |
| JSON-RAPIDCACHES-001 | storage.rapidCaches.disable | POST /storage/v1/b/{bucket}/rapidCaches/{rapidCacheId}/disable |
| JSON-RAPIDCACHES-002 | storage.rapidCaches.get | GET /storage/v1/b/{bucket}/rapidCaches/{rapidCacheId} |
| JSON-RAPIDCACHES-003 | storage.rapidCaches.insert | POST /storage/v1/b/{bucket}/rapidCaches |
| JSON-RAPIDCACHES-004 | storage.rapidCaches.list | GET /storage/v1/b/{bucket}/rapidCaches |
| JSON-RAPIDCACHES-005 | storage.rapidCaches.update | PATCH /storage/v1/b/{bucket}/rapidCaches/{rapidCacheId} |

## gRPC Storage v2

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| RPC-STORAGE-001 | google.storage.v2.Storage/DeleteBucket | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-002 | google.storage.v2.Storage/GetBucket | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-003 | google.storage.v2.Storage/CreateBucket | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-004 | google.storage.v2.Storage/ListBuckets | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-005 | google.storage.v2.Storage/LockBucketRetentionPolicy | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-006 | google.storage.v2.Storage/GetIamPolicy | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-007 | google.storage.v2.Storage/SetIamPolicy | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-008 | google.storage.v2.Storage/TestIamPermissions | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-009 | google.storage.v2.Storage/UpdateBucket | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-010 | google.storage.v2.Storage/ComposeObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-011 | google.storage.v2.Storage/DeleteObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-012 | google.storage.v2.Storage/RestoreObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-013 | google.storage.v2.Storage/CancelResumableWrite | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-014 | google.storage.v2.Storage/GetObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-015 | google.storage.v2.Storage/ReadObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-016 | google.storage.v2.Storage/BidiReadObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-017 | google.storage.v2.Storage/UpdateObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-018 | google.storage.v2.Storage/WriteObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-019 | google.storage.v2.Storage/BidiWriteObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-020 | google.storage.v2.Storage/ListObjects | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-021 | google.storage.v2.Storage/RewriteObject | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-022 | google.storage.v2.Storage/StartResumableWrite | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-023 | google.storage.v2.Storage/QueryWriteStatus | See the typed RPC inventory and HTTP annotations where present. |
| RPC-STORAGE-024 | google.storage.v2.Storage/MoveObject | See the typed RPC inventory and HTTP annotations where present. |

## FeatureConfigs v2 REST

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| JSON-FEATURECONFIG-001 | FeatureConfigs.create | FeatureConfigs.create |
| JSON-FEATURECONFIG-002 | FeatureConfigs.delete | FeatureConfigs.delete |
| JSON-FEATURECONFIG-003 | FeatureConfigs.get | FeatureConfigs.get |
| JSON-FEATURECONFIG-004 | FeatureConfigs.list | FeatureConfigs.list |
| JSON-FEATURECONFIG-005 | FeatureConfigs.patch | FeatureConfigs.patch |

## XML API

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-XML-001 | GET Service | GET Service / list buckets |
| EXT-XML-002 | DELETE Bucket | DELETE Bucket |
| EXT-XML-003 | GET Bucket | GET Bucket / list objects |
| EXT-XML-004 | GET Bucket ?uploads | GET Bucket / list multipart uploads |
| EXT-XML-005 | GET Bucket ?acl | GET Bucket ACL |
| EXT-XML-006 | HEAD Bucket | HEAD Bucket |
| EXT-XML-007 | POST Bucket ?delete | POST Bucket / multi-object delete |
| EXT-XML-008 | PUT Bucket | PUT Bucket / create |
| EXT-XML-009 | PUT Bucket ?acl | PUT Bucket ACL |
| EXT-XML-010 | DELETE Object | DELETE Object |
| EXT-XML-011 | DELETE Object ?uploadId | Abort multipart upload |
| EXT-XML-012 | GET Object | GET Object / download |
| EXT-XML-013 | GET Object ?uploadId | GET Object / list parts |
| EXT-XML-014 | GET Object ?acl | GET Object ACL |
| EXT-XML-015 | GET Object ?encryption | GET Object encryption |
| EXT-XML-016 | GET Object ?retention | GET Object retention |
| EXT-XML-017 | HEAD Object | HEAD Object |
| EXT-XML-018 | POST Object; x-goog-resumable:start | Initiate resumable upload |
| EXT-XML-019 | POST Object ?uploads | Initiate XML multipart upload |
| EXT-XML-020 | POST Object ?uploadId | Complete XML multipart upload |
| EXT-XML-021 | POST /{bucket}; multipart/form-data; object name in form field key (path style) | HTML form upload |
| EXT-XML-022 | PUT Object | PUT Object / upload |
| EXT-XML-023 | PUT Object ?partNumber&uploadId | Upload XML multipart part |
| EXT-XML-024 | PUT Object; x-goog-copy-source | Copy Object |
| EXT-XML-025 | PUT Object ?compose | Compose Object |
| EXT-XML-026 | PUT Object ?acl | PUT Object ACL |
| EXT-XML-027 | PUT Object ?retention | PUT Object retention |
| EXT-XML-028 | GET /?Action=ListAccessKeys | List HMAC keys |
| EXT-XML-029 | POST /?Action=CreateAccessKey&UserName={serviceAccountEmail} | Create HMAC key |
| EXT-XML-030 | POST /?Action=UpdateAccessKey&AccessKeyId={id}&Status={state} | Update HMAC key |
| EXT-XML-031 | POST /?Action=DeleteAccessKey&AccessKeyId={id} | Delete HMAC key |
| EXT-XML-032 | GET Bucket ?billing | Get bucket billing |
| EXT-XML-033 | GET Bucket ?cors | Get bucket cors |
| EXT-XML-034 | GET Bucket ?customPlacementConfig | Get bucket customPlacementConfig |
| EXT-XML-035 | GET Bucket ?encryptionConfig | Get bucket encryptionConfig |
| EXT-XML-036 | GET Bucket ?lifecycle | Get bucket lifecycle |
| EXT-XML-037 | GET Bucket ?location | Get bucket location |
| EXT-XML-038 | GET Bucket ?logging | Get bucket logging |
| EXT-XML-039 | GET Bucket ?object-lock | Get bucket object-lock |
| EXT-XML-040 | GET Bucket ?storageClass | Get bucket storageClass |
| EXT-XML-041 | GET Bucket ?tagging | Get bucket tagging |
| EXT-XML-042 | GET Bucket ?versioning | Get bucket versioning |
| EXT-XML-043 | GET Bucket ?websiteConfig | Get bucket websiteConfig |
| EXT-XML-044 | PUT Bucket ?billing | Set bucket billing |
| EXT-XML-045 | PUT Bucket ?cors | Set bucket cors |
| EXT-XML-046 | PUT Bucket ?encryptionConfig | Set bucket encryptionConfig |
| EXT-XML-047 | PUT Bucket ?lifecycle | Set bucket lifecycle |
| EXT-XML-048 | PUT Bucket ?logging | Set bucket logging |
| EXT-XML-049 | PUT Bucket ?storageClass | Set bucket storageClass |
| EXT-XML-050 | PUT Bucket ?tagging | Set bucket tagging |
| EXT-XML-051 | PUT Bucket ?versioning | Set bucket versioning |
| EXT-XML-052 | PUT Bucket ?websiteConfig | Set bucket websiteConfig |

## Storage Control v2

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-CTRL-001 | google.storage.control.v2.StorageControl/CreateAnywhereCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-002 | google.storage.control.v2.StorageControl/CreateFolder | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-003 | google.storage.control.v2.StorageControl/CreateManagedFolder | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-004 | google.storage.control.v2.StorageControl/DeleteFolder | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-005 | google.storage.control.v2.StorageControl/DeleteManagedFolder | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-006 | google.storage.control.v2.StorageControl/DisableAnywhereCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-007 | google.storage.control.v2.StorageControl/GetAnywhereCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-008 | google.storage.control.v2.StorageControl/GetFolder | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-009 | google.storage.control.v2.StorageControl/GetFolderIntelligenceConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-010 | google.storage.control.v2.StorageControl/GetManagedFolder | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-011 | google.storage.control.v2.StorageControl/GetOrganizationIntelligenceConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-012 | google.storage.control.v2.StorageControl/GetProjectIntelligenceConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-013 | google.storage.control.v2.StorageControl/GetStorageLayout | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-014 | google.storage.control.v2.StorageControl/ListAnywhereCaches | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-015 | google.storage.control.v2.StorageControl/ListFolders | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-016 | google.storage.control.v2.StorageControl/ListManagedFolders | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-017 | google.storage.control.v2.StorageControl/PauseAnywhereCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-018 | google.storage.control.v2.StorageControl/RenameFolder | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-019 | google.storage.control.v2.StorageControl/ResumeAnywhereCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-020 | google.storage.control.v2.StorageControl/UpdateAnywhereCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-021 | google.storage.control.v2.StorageControl/UpdateFolderIntelligenceConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-022 | google.storage.control.v2.StorageControl/UpdateOrganizationIntelligenceConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-023 | google.storage.control.v2.StorageControl/UpdateProjectIntelligenceConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-024 | google.storage.control.v2.StorageControl/DeleteFolderRecursive | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-025 | google.storage.control.v2.StorageControl/UpdateManagedFolder | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-026 | google.storage.control.v2.StorageControl/CreateRapidCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-027 | google.storage.control.v2.StorageControl/UpdateRapidCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-028 | google.storage.control.v2.StorageControl/DisableRapidCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-029 | google.storage.control.v2.StorageControl/GetRapidCache | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-030 | google.storage.control.v2.StorageControl/ListRapidCaches | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-031 | google.storage.control.v2.StorageControl/GetIamPolicy | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-032 | google.storage.control.v2.StorageControl/SetIamPolicy | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-033 | google.storage.control.v2.StorageControl/TestIamPermissions | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-034 | google.storage.control.v2.StorageControl/GetIntelligenceFinding | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-035 | google.storage.control.v2.StorageControl/ListIntelligenceFindings | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-036 | google.storage.control.v2.StorageControl/SummarizeIntelligenceFindings | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-037 | google.storage.control.v2.StorageControl/GetIntelligenceFindingRevision | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-038 | google.storage.control.v2.StorageControl/ListIntelligenceFindingRevisions | See the typed RPC inventory and HTTP annotations where present. |
| EXT-CTRL-039 | google.storage.control.v2.StorageControl/ViewObjectFullContext | See the typed RPC inventory and HTTP annotations where present. |

## Storage Insights

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-INS-001 | storageinsights.googleapis.com/v1 projects.locations.get | projects.locations.get |
| EXT-INS-002 | storageinsights.googleapis.com/v1 projects.locations.list | projects.locations.list |
| EXT-INS-003 | storageinsights.googleapis.com/v1 projects.locations.datasetConfigs.create | projects.locations.datasetConfigs.create |
| EXT-INS-004 | storageinsights.googleapis.com/v1 projects.locations.datasetConfigs.delete | projects.locations.datasetConfigs.delete |
| EXT-INS-005 | storageinsights.googleapis.com/v1 projects.locations.datasetConfigs.get | projects.locations.datasetConfigs.get |
| EXT-INS-006 | storageinsights.googleapis.com/v1 projects.locations.datasetConfigs.linkDataset | projects.locations.datasetConfigs.linkDataset |
| EXT-INS-007 | storageinsights.googleapis.com/v1 projects.locations.datasetConfigs.list | projects.locations.datasetConfigs.list |
| EXT-INS-008 | storageinsights.googleapis.com/v1 projects.locations.datasetConfigs.patch | projects.locations.datasetConfigs.patch |
| EXT-INS-009 | storageinsights.googleapis.com/v1 projects.locations.datasetConfigs.unlinkDataset | projects.locations.datasetConfigs.unlinkDataset |
| EXT-INS-010 | storageinsights.googleapis.com/v1 projects.locations.operations.cancel | projects.locations.operations.cancel |
| EXT-INS-011 | storageinsights.googleapis.com/v1 projects.locations.operations.delete | projects.locations.operations.delete |
| EXT-INS-012 | storageinsights.googleapis.com/v1 projects.locations.operations.get | projects.locations.operations.get |
| EXT-INS-013 | storageinsights.googleapis.com/v1 projects.locations.operations.list | projects.locations.operations.list |
| EXT-INS-014 | storageinsights.googleapis.com/v1 projects.locations.reportConfigs.create | projects.locations.reportConfigs.create |
| EXT-INS-015 | storageinsights.googleapis.com/v1 projects.locations.reportConfigs.delete | projects.locations.reportConfigs.delete |
| EXT-INS-016 | storageinsights.googleapis.com/v1 projects.locations.reportConfigs.get | projects.locations.reportConfigs.get |
| EXT-INS-017 | storageinsights.googleapis.com/v1 projects.locations.reportConfigs.list | projects.locations.reportConfigs.list |
| EXT-INS-018 | storageinsights.googleapis.com/v1 projects.locations.reportConfigs.patch | projects.locations.reportConfigs.patch |
| EXT-INS-019 | storageinsights.googleapis.com/v1 projects.locations.reportConfigs.reportDetails.get | projects.locations.reportConfigs.reportDetails.get |
| EXT-INS-020 | storageinsights.googleapis.com/v1 projects.locations.reportConfigs.reportDetails.list | projects.locations.reportConfigs.reportDetails.list |

## Storage Batch Operations

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-BATCH-001 | storagebatchoperations.googleapis.com/v1 projects.locations.get | projects.locations.get |
| EXT-BATCH-002 | storagebatchoperations.googleapis.com/v1 projects.locations.list | projects.locations.list |
| EXT-BATCH-003 | storagebatchoperations.googleapis.com/v1 projects.locations.jobs.cancel | projects.locations.jobs.cancel |
| EXT-BATCH-004 | storagebatchoperations.googleapis.com/v1 projects.locations.jobs.create | projects.locations.jobs.create |
| EXT-BATCH-005 | storagebatchoperations.googleapis.com/v1 projects.locations.jobs.delete | projects.locations.jobs.delete |
| EXT-BATCH-006 | storagebatchoperations.googleapis.com/v1 projects.locations.jobs.get | projects.locations.jobs.get |
| EXT-BATCH-007 | storagebatchoperations.googleapis.com/v1 projects.locations.jobs.list | projects.locations.jobs.list |
| EXT-BATCH-008 | storagebatchoperations.googleapis.com/v1 projects.locations.jobs.bucketOperations.get | projects.locations.jobs.bucketOperations.get |
| EXT-BATCH-009 | storagebatchoperations.googleapis.com/v1 projects.locations.jobs.bucketOperations.list | projects.locations.jobs.bucketOperations.list |
| EXT-BATCH-010 | storagebatchoperations.googleapis.com/v1 projects.locations.operations.cancel | projects.locations.operations.cancel |
| EXT-BATCH-011 | storagebatchoperations.googleapis.com/v1 projects.locations.operations.delete | projects.locations.operations.delete |
| EXT-BATCH-012 | storagebatchoperations.googleapis.com/v1 projects.locations.operations.get | projects.locations.operations.get |
| EXT-BATCH-013 | storagebatchoperations.googleapis.com/v1 projects.locations.operations.list | projects.locations.operations.list |

## Storage Intelligence

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-INT-001 | GET /v2/projects/{id}/locations/global/intelligenceConfig | projects.locations.getIntelligenceConfig |
| EXT-INT-002 | PATCH /v2/projects/{id}/locations/global/intelligenceConfig | projects.locations.updateIntelligenceConfig |
| EXT-INT-003 | GET /v2/folders/{id}/locations/global/intelligenceConfig | folders.locations.getIntelligenceConfig |
| EXT-INT-004 | PATCH /v2/folders/{id}/locations/global/intelligenceConfig | folders.locations.updateIntelligenceConfig |
| EXT-INT-005 | GET /v2/organizations/{id}/locations/global/intelligenceConfig | organizations.locations.getIntelligenceConfig |
| EXT-INT-006 | PATCH /v2/organizations/{id}/locations/global/intelligenceConfig | organizations.locations.updateIntelligenceConfig |
| EXT-INT-007 | GET /v2/projects/{project}/locations/{location}/intelligenceFindings/{finding} | projects.locations.intelligenceFindings.get |
| EXT-INT-008 | GET /v2/projects/{project}/locations/{location}/intelligenceFindings | projects.locations.intelligenceFindings.list |
| EXT-INT-009 | GET /v2/{projects\|folders\|organizations}/{id}/locations/{location}/intelligenceFindings:summarize | projects.locations.intelligenceFindings.summarize |
| EXT-INT-010 | GET /v2/projects/{project}/locations/{location}/intelligenceFindings/{finding}/revisions | projects.locations.intelligenceFindings.revisions.list |
| EXT-INT-011 | GET /v2/{name=projects/*/locations/*/intelligenceFindings/*/revisions/*} | projects.locations.intelligenceFindings.revisions.get |

## Cloud Storage MCP

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-MCP-001 | POST /storage/mcp tools/call:list_buckets | list_buckets |
| EXT-MCP-002 | POST /storage/mcp tools/call:list_objects | list_objects |
| EXT-MCP-003 | POST /storage/mcp tools/call:create_bucket | create_bucket |
| EXT-MCP-004 | POST /storage/mcp tools/call:delete_bucket | delete_bucket |
| EXT-MCP-005 | POST /storage/mcp tools/call:read_text | read_text |
| EXT-MCP-006 | POST /storage/mcp tools/call:read_object | read_object |
| EXT-MCP-007 | POST /storage/mcp tools/call:write_text | write_text |
| EXT-MCP-008 | POST /storage/mcp tools/call:delete_object | delete_object |
| EXT-MCP-009 | POST /storage/mcp tools/call:get_object_metadata | get_object_metadata |

## Shared long-running RPC

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-LRO-001 | google.longrunning.Operations/CancelOperation | CancelOperation |
| EXT-LRO-002 | google.longrunning.Operations/DeleteOperation | DeleteOperation |
| EXT-LRO-003 | google.longrunning.Operations/GetOperation | GetOperation |
| EXT-LRO-004 | google.longrunning.Operations/ListOperations | ListOperations |
| EXT-LRO-005 | google.longrunning.Operations/WaitOperation | WaitOperation |

## Storage Insights RPC

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-INS-RPC-001 | google.cloud.storageinsights.v1.StorageInsights/ListReportConfigs | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-002 | google.cloud.storageinsights.v1.StorageInsights/GetReportConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-003 | google.cloud.storageinsights.v1.StorageInsights/CreateReportConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-004 | google.cloud.storageinsights.v1.StorageInsights/UpdateReportConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-005 | google.cloud.storageinsights.v1.StorageInsights/DeleteReportConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-006 | google.cloud.storageinsights.v1.StorageInsights/ListReportDetails | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-007 | google.cloud.storageinsights.v1.StorageInsights/GetReportDetail | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-008 | google.cloud.storageinsights.v1.StorageInsights/ListDatasetConfigs | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-009 | google.cloud.storageinsights.v1.StorageInsights/GetDatasetConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-010 | google.cloud.storageinsights.v1.StorageInsights/CreateDatasetConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-011 | google.cloud.storageinsights.v1.StorageInsights/UpdateDatasetConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-012 | google.cloud.storageinsights.v1.StorageInsights/DeleteDatasetConfig | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-013 | google.cloud.storageinsights.v1.StorageInsights/LinkDataset | See the typed RPC inventory and HTTP annotations where present. |
| EXT-INS-RPC-014 | google.cloud.storageinsights.v1.StorageInsights/UnlinkDataset | See the typed RPC inventory and HTTP annotations where present. |

## Storage Batch Operations RPC

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-BATCH-RPC-001 | google.cloud.storagebatchoperations.v1.StorageBatchOperations/ListJobs | See the typed RPC inventory and HTTP annotations where present. |
| EXT-BATCH-RPC-002 | google.cloud.storagebatchoperations.v1.StorageBatchOperations/GetJob | See the typed RPC inventory and HTTP annotations where present. |
| EXT-BATCH-RPC-003 | google.cloud.storagebatchoperations.v1.StorageBatchOperations/CreateJob | See the typed RPC inventory and HTTP annotations where present. |
| EXT-BATCH-RPC-004 | google.cloud.storagebatchoperations.v1.StorageBatchOperations/DeleteJob | See the typed RPC inventory and HTTP annotations where present. |
| EXT-BATCH-RPC-005 | google.cloud.storagebatchoperations.v1.StorageBatchOperations/CancelJob | See the typed RPC inventory and HTTP annotations where present. |
| EXT-BATCH-RPC-006 | google.cloud.storagebatchoperations.v1.StorageBatchOperations/ListBucketOperations | See the typed RPC inventory and HTTP annotations where present. |
| EXT-BATCH-RPC-007 | google.cloud.storagebatchoperations.v1.StorageBatchOperations/GetBucketOperation | See the typed RPC inventory and HTTP annotations where present. |

## Shared location RPC

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-LOC-RPC-001 | storageinsights.googleapis.com: google.cloud.location.Locations/ListLocations | storageinsights.googleapis.com ListLocations |
| EXT-LOC-RPC-002 | storageinsights.googleapis.com: google.cloud.location.Locations/GetLocation | storageinsights.googleapis.com GetLocation |
| EXT-LOC-RPC-003 | storagebatchoperations.googleapis.com: google.cloud.location.Locations/ListLocations | storagebatchoperations.googleapis.com ListLocations |
| EXT-LOC-RPC-004 | storagebatchoperations.googleapis.com: google.cloud.location.Locations/GetLocation | storagebatchoperations.googleapis.com GetLocation |

## Storage Control v2 REST binding

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| EXT-CTRL-REST-001 | POST /v2/{parent=projects/*/buckets/*}/folders | CreateFolder HTTP binding |
| EXT-CTRL-REST-002 | DELETE /v2/{name=projects/*/buckets/*/folders/**} | DeleteFolder HTTP binding |
| EXT-CTRL-REST-003 | GET /v2/{name=projects/*/buckets/*/folders/**} | GetFolder HTTP binding |
| EXT-CTRL-REST-004 | GET /v2/{parent=projects/*/buckets/*}/folders | ListFolders HTTP binding |
| EXT-CTRL-REST-005 | POST /v2/{name=projects/*/buckets/*/folders/**}:rename | RenameFolder HTTP binding |
| EXT-CTRL-REST-006 | POST /v2/{name=projects/*/buckets/*/folders/**}:deleteRecursive | DeleteFolderRecursive HTTP binding |
| EXT-CTRL-REST-007 | GET /v2/{name=projects/*/buckets/*/storageLayout} | GetStorageLayout HTTP binding |

## Storage Control REST

| Feature ID | Method / interface | Route or detail |
| --- | --- | --- |
| AUDIT-023 | GET /v2/{name=projects/*/buckets/*/objects/**}:viewFullContext | GET /v2/{name=projects/*/buckets/*/objects/**}:viewFullContext |

## Representation coverage

- `compatibility/upstream/json-methods.json`: verb, path, parameters, request/response schema refs and upload/download capabilities for 87 methods.
- `compatibility/upstream/json-schemas.json`: factual field/type/enum/ref nesting for all 38 schemas and global query parameters.
- `compatibility/schema-field-coverage.json`: 357 property locations; selected Bucket PATCH fields have scoped acceptance cases, with broader method coverage pending.
- `compatibility/parameter-coverage.json`: 401 global/per-method path/query parameter locations; buckets.patch parameters have scoped cases and explicit unsupported semantics.
- `compatibility/upstream/storage-rpc-methods.json`:24 Storage v2 signatures and streaming flags.
- `compatibility/upstream/auxiliary-rpc-methods.json`:60 direct Control/Insights/Batch signatures, typed LRO metadata, HTTP bindings and shared service registrations.

XML headers/elements, protobuf fields, IAM permissions, launch-stage restrictions, error variants and feature cross-products must be expanded into acceptance cases for each selected slice. The present schema inventory is complete for the fetched JSON schema definitions, not a field-by-field inventory of every service in this broader plan.
