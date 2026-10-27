# Feature implementation matrix

Generated from `compatibility/features.json`; do not edit status here.

Research cutoff: 2026-10-23. Implementation status and achieved fidelity are independent.

API operations, behaviors and project requirements have separate denominators. Parameter and schema-field inventories are not additional features. SDK evidence is recorded separately on each row.

`Kind` groups rows by what they track: `api_operation` is an API method or operation, `behavior` is a semantic rule or workflow, and `project_requirement` is an emulator engineering or usability requirement.

## Implementation status

Status tracks completion of each row's declared acceptance cases.

| Kind | Rows | NOT_STARTED | IN_PROGRESS | PARTIAL | IMPLEMENTED | BLOCKED | NOT_APPLICABLE |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| api_operation | 298 | 276 | 0 | 21 | 1 | 0 | 0 |
| behavior | 201 | 124 | 0 | 39 | 38 | 0 | 0 |
| project_requirement | 92 | 35 | 0 | 28 | 29 | 0 | 0 |

## Achieved fidelity

Fidelity describes how the feature is reproduced. These categories are not a completion percentage or a ranking. The counts below use achieved fidelity (`actual_fidelity`); target fidelity is the intended result.

| Fidelity | Meaning |
| --- | --- |
| `NONE` | No achieved fidelity has been established yet. |
| `EXACT` | Observable Google API behavior is reproduced within the explicitly tested scope and profile. |
| `LOCAL_SIMULATION` | A local model simulates service or infrastructure behavior; it does not provide Google's real infrastructure guarantees. |
| `METADATA_ONLY` | Settings or fields can be stored and read back, but their effects are not enforced. |
| `EXTERNAL` | Side effects require a separately provided external service or integration. |
| `PROJECT` | An emulator engineering or usability requirement, rather than a Google API compatibility claim. |

Status and fidelity answer different questions. A `PARTIAL` row can have `EXACT` fidelity for its tested subset while other acceptance cases remain pending. An `IMPLEMENTED` local simulation still has `LOCAL_SIMULATION` fidelity; it does not become `EXACT`.

| Kind | NONE | EXACT | LOCAL_SIMULATION | METADATA_ONLY | EXTERNAL | PROJECT |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| api_operation | 276 | 6 | 16 | 0 | 0 | 0 |
| behavior | 124 | 44 | 30 | 3 | 0 | 0 |
| project_requirement | 35 | 0 | 0 | 0 | 0 | 57 |

## api_operation

### Cloud Storage MCP

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-MCP-001 | list&#95;buckets | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-002 | list&#95;objects | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-003 | create&#95;bucket | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-004 | delete&#95;bucket | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-005 | read&#95;text | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-006 | read&#95;object | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-007 | write&#95;text | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-008 | delete&#95;object | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-009 | get&#95;object&#95;metadata | NOT_STARTED | P7 | EXACT | NONE | 0 |

### FeatureConfigs

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-FEATURECONFIG-001 | FeatureConfigs.create | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-FEATURECONFIG-002 | FeatureConfigs.delete | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-FEATURECONFIG-003 | FeatureConfigs.get | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-FEATURECONFIG-004 | FeatureConfigs.list | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-FEATURECONFIG-005 | FeatureConfigs.patch | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### JSON API / anywhereCaches

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-ANYWHERECACHES-001 | anywhereCaches.disable | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-ANYWHERECACHES-002 | anywhereCaches.get | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-ANYWHERECACHES-003 | anywhereCaches.insert | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-ANYWHERECACHES-004 | anywhereCaches.list | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-ANYWHERECACHES-005 | anywhereCaches.pause | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-ANYWHERECACHES-006 | anywhereCaches.resume | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-ANYWHERECACHES-007 | anywhereCaches.update | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### JSON API / bucketAccessControls

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-BUCKETACCESSCONTROLS-001 | bucketAccessControls.delete | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-BUCKETACCESSCONTROLS-002 | bucketAccessControls.get | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-BUCKETACCESSCONTROLS-003 | bucketAccessControls.insert | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-BUCKETACCESSCONTROLS-004 | bucketAccessControls.list | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-BUCKETACCESSCONTROLS-005 | bucketAccessControls.patch | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-BUCKETACCESSCONTROLS-006 | bucketAccessControls.update | NOT_STARTED | P2 | EXACT | NONE | 0 |

### JSON API / buckets

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-BUCKETS-001 | buckets.delete | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 5 |
| JSON-BUCKETS-002 | buckets.get | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 6 |
| JSON-BUCKETS-003 | buckets.getIamPolicy | PARTIAL | P2 | EXACT | EXACT | 3 |
| JSON-BUCKETS-004 | buckets.getStorageLayout | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-BUCKETS-005 | buckets.insert | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 5 |
| JSON-BUCKETS-006 | buckets.list | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| JSON-BUCKETS-007 | buckets.lockRetentionPolicy | NOT_STARTED | P4 | EXACT | NONE | 0 |
| JSON-BUCKETS-008 | buckets.patch | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 4 |
| JSON-BUCKETS-009 | buckets.relocate | NOT_STARTED | P7 | EXACT | NONE | 0 |
| JSON-BUCKETS-010 | buckets.restore | NOT_STARTED | P4 | EXACT | NONE | 0 |
| JSON-BUCKETS-011 | buckets.setIamPolicy | PARTIAL | P2 | EXACT | EXACT | 2 |
| JSON-BUCKETS-012 | buckets.testIamPermissions | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 2 |
| JSON-BUCKETS-013 | buckets.update | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 4 |

### JSON API / defaultObjectAccessControls

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-DEFAULTOBJECTACCESSCONTROLS-001 | defaultObjectAccessControls.delete | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-DEFAULTOBJECTACCESSCONTROLS-002 | defaultObjectAccessControls.get | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-DEFAULTOBJECTACCESSCONTROLS-003 | defaultObjectAccessControls.insert | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-DEFAULTOBJECTACCESSCONTROLS-004 | defaultObjectAccessControls.list | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-DEFAULTOBJECTACCESSCONTROLS-005 | defaultObjectAccessControls.patch | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-DEFAULTOBJECTACCESSCONTROLS-006 | defaultObjectAccessControls.update | NOT_STARTED | P2 | EXACT | NONE | 0 |

### JSON API / folders

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-FOLDERS-001 | folders.delete | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-FOLDERS-002 | folders.deleteRecursive | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-FOLDERS-003 | folders.get | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-FOLDERS-004 | folders.insert | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-FOLDERS-005 | folders.list | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-FOLDERS-006 | folders.rename | NOT_STARTED | P5 | EXACT | NONE | 0 |

### JSON API / managedFolders

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-MANAGEDFOLDERS-001 | managedFolders.delete | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-MANAGEDFOLDERS-002 | managedFolders.get | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-MANAGEDFOLDERS-003 | managedFolders.getIamPolicy | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-MANAGEDFOLDERS-004 | managedFolders.insert | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-MANAGEDFOLDERS-005 | managedFolders.list | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-MANAGEDFOLDERS-006 | managedFolders.setIamPolicy | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-MANAGEDFOLDERS-007 | managedFolders.testIamPermissions | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-MANAGEDFOLDERS-008 | managedFolders.update | NOT_STARTED | P5 | EXACT | NONE | 0 |

### JSON API / notifications

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-NOTIFICATIONS-001 | notifications.delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| JSON-NOTIFICATIONS-002 | notifications.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| JSON-NOTIFICATIONS-003 | notifications.insert | NOT_STARTED | P7 | EXACT | NONE | 0 |
| JSON-NOTIFICATIONS-004 | notifications.list | NOT_STARTED | P7 | EXACT | NONE | 0 |

### JSON API / objectAccessControls

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-OBJECTACCESSCONTROLS-001 | objectAccessControls.delete | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTACCESSCONTROLS-002 | objectAccessControls.get | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTACCESSCONTROLS-003 | objectAccessControls.insert | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTACCESSCONTROLS-004 | objectAccessControls.list | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTACCESSCONTROLS-005 | objectAccessControls.patch | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTACCESSCONTROLS-006 | objectAccessControls.update | NOT_STARTED | P2 | EXACT | NONE | 0 |

### JSON API / objects

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-OBJECTS-001 | objects.bulkRestore | NOT_STARTED | P4 | EXACT | NONE | 0 |
| JSON-OBJECTS-002 | objects.compose | IMPLEMENTED | P3 | EXACT | EXACT | 2 |
| JSON-OBJECTS-003 | objects.copy | PARTIAL | P3 | EXACT | EXACT | 1 |
| JSON-OBJECTS-004 | objects.delete | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 5 |
| JSON-OBJECTS-005 | objects.get | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 7 |
| JSON-OBJECTS-006 | objects.getIamPolicy | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTS-007 | objects.insert | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 7 |
| JSON-OBJECTS-008 | objects.list | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 4 |
| JSON-OBJECTS-009 | objects.move | PARTIAL | P3 | EXACT | EXACT | 2 |
| JSON-OBJECTS-010 | objects.patch | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 5 |
| JSON-OBJECTS-011 | objects.restore | NOT_STARTED | P4 | EXACT | NONE | 0 |
| JSON-OBJECTS-012 | objects.rewrite | PARTIAL | P3 | EXACT | EXACT | 1 |
| JSON-OBJECTS-013 | objects.setIamPolicy | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTS-014 | objects.testIamPermissions | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTS-015 | objects.update | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 4 |
| JSON-OBJECTS-016 | objects.viewFullContext | NOT_STARTED | P7 | EXACT | NONE | 0 |

### JSON API / operations

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-OPERATIONS-001 | operations.advanceRelocateBucket | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-OPERATIONS-002 | operations.cancel | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-OPERATIONS-003 | operations.get | NOT_STARTED | P5 | EXACT | NONE | 0 |
| JSON-OPERATIONS-004 | operations.list | NOT_STARTED | P5 | EXACT | NONE | 0 |

### JSON API / projects.hmacKeys

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-PROJECTS-HMACKEYS-001 | projects.hmacKeys.create | NOT_STARTED | P3 | EXACT | NONE | 0 |
| JSON-PROJECTS-HMACKEYS-002 | projects.hmacKeys.delete | NOT_STARTED | P3 | EXACT | NONE | 0 |
| JSON-PROJECTS-HMACKEYS-003 | projects.hmacKeys.get | NOT_STARTED | P3 | EXACT | NONE | 0 |
| JSON-PROJECTS-HMACKEYS-004 | projects.hmacKeys.list | NOT_STARTED | P3 | EXACT | NONE | 0 |
| JSON-PROJECTS-HMACKEYS-005 | projects.hmacKeys.update | NOT_STARTED | P3 | EXACT | NONE | 0 |

### JSON API / projects.serviceAccount

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-PROJECTS-SERVICEACCOUNT-001 | projects.serviceAccount.get | NOT_STARTED | P2 | EXACT | NONE | 0 |

### JSON API / rapidCaches

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| JSON-RAPIDCACHES-001 | rapidCaches.disable | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-RAPIDCACHES-002 | rapidCaches.get | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-RAPIDCACHES-003 | rapidCaches.insert | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-RAPIDCACHES-004 | rapidCaches.list | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| JSON-RAPIDCACHES-005 | rapidCaches.update | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### Shared location RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-LOC-RPC-001 | storageinsights.googleapis.com ListLocations | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-LOC-RPC-002 | storageinsights.googleapis.com GetLocation | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-LOC-RPC-003 | storagebatchoperations.googleapis.com ListLocations | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-LOC-RPC-004 | storagebatchoperations.googleapis.com GetLocation | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Shared long-running RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-LRO-001 | CancelOperation | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-LRO-002 | DeleteOperation | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-LRO-003 | GetOperation | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-LRO-004 | ListOperations | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-LRO-005 | WaitOperation | NOT_STARTED | P5 | EXACT | NONE | 0 |

### Storage Batch Operations

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BATCH-001 | projects.locations.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-002 | projects.locations.list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-003 | projects.locations.jobs.cancel | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-004 | projects.locations.jobs.create | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-005 | projects.locations.jobs.delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-006 | projects.locations.jobs.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-007 | projects.locations.jobs.list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-008 | projects.locations.jobs.bucketOperations.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-009 | projects.locations.jobs.bucketOperations.list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-010 | projects.locations.operations.cancel | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-011 | projects.locations.operations.delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-012 | projects.locations.operations.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-013 | projects.locations.operations.list | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Batch Operations RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BATCH-RPC-001 | ListJobs | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-002 | GetJob | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-003 | CreateJob | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-004 | DeleteJob | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-005 | CancelJob | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-006 | ListBucketOperations | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-007 | GetBucketOperation | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Control REST

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-023 | ViewObjectFullContext v2 HTTP binding | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Control v2

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-CTRL-001 | CreateAnywhereCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-002 | CreateFolder | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-003 | CreateManagedFolder | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-004 | DeleteFolder | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-005 | DeleteManagedFolder | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-006 | DisableAnywhereCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-007 | GetAnywhereCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-008 | GetFolder | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-009 | GetFolderIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-010 | GetManagedFolder | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-011 | GetOrganizationIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-012 | GetProjectIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-013 | GetStorageLayout | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-014 | ListAnywhereCaches | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-015 | ListFolders | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-016 | ListManagedFolders | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-017 | PauseAnywhereCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-018 | RenameFolder | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-019 | ResumeAnywhereCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-020 | UpdateAnywhereCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-021 | UpdateFolderIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-022 | UpdateOrganizationIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-023 | UpdateProjectIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-024 | DeleteFolderRecursive | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-025 | UpdateManagedFolder | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-026 | CreateRapidCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-027 | UpdateRapidCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-028 | DisableRapidCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-029 | GetRapidCache | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-030 | ListRapidCaches | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-031 | GetIamPolicy | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-032 | SetIamPolicy | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-033 | TestIamPermissions | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-034 | GetIntelligenceFinding | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-035 | ListIntelligenceFindings | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-036 | SummarizeIntelligenceFindings | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-037 | GetIntelligenceFindingRevision | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-038 | ListIntelligenceFindingRevisions | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-039 | ViewObjectFullContext | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Control v2 REST binding

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-CTRL-REST-001 | CreateFolder HTTP binding | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-002 | DeleteFolder HTTP binding | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-003 | GetFolder HTTP binding | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-004 | ListFolders HTTP binding | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-005 | RenameFolder HTTP binding | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-006 | DeleteFolderRecursive HTTP binding | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-007 | GetStorageLayout HTTP binding | NOT_STARTED | P5 | EXACT | NONE | 0 |

### Storage Insights

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-INS-001 | projects.locations.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-002 | projects.locations.list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-003 | projects.locations.datasetConfigs.create | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-004 | projects.locations.datasetConfigs.delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-005 | projects.locations.datasetConfigs.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-006 | projects.locations.datasetConfigs.linkDataset | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INS-007 | projects.locations.datasetConfigs.list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-008 | projects.locations.datasetConfigs.patch | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-009 | projects.locations.datasetConfigs.unlinkDataset | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INS-010 | projects.locations.operations.cancel | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-011 | projects.locations.operations.delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-012 | projects.locations.operations.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-013 | projects.locations.operations.list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-014 | projects.locations.reportConfigs.create | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-015 | projects.locations.reportConfigs.delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-016 | projects.locations.reportConfigs.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-017 | projects.locations.reportConfigs.list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-018 | projects.locations.reportConfigs.patch | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-019 | projects.locations.reportConfigs.reportDetails.get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-020 | projects.locations.reportConfigs.reportDetails.list | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Insights RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-INS-RPC-001 | ListReportConfigs | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-002 | GetReportConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-003 | CreateReportConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-004 | UpdateReportConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-005 | DeleteReportConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-006 | ListReportDetails | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-007 | GetReportDetail | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-008 | ListDatasetConfigs | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-009 | GetDatasetConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-010 | CreateDatasetConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-011 | UpdateDatasetConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-012 | DeleteDatasetConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-013 | LinkDataset | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INS-RPC-014 | UnlinkDataset | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### Storage Intelligence

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-INT-001 | projects.locations.getIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-002 | projects.locations.updateIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-003 | folders.locations.getIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-004 | folders.locations.updateIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-005 | organizations.locations.getIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-006 | organizations.locations.updateIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-007 | projects.locations.intelligenceFindings.get | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INT-008 | projects.locations.intelligenceFindings.list | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INT-009 | projects.locations.intelligenceFindings.summarize | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INT-010 | projects.locations.intelligenceFindings.revisions.list | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INT-011 | projects.locations.intelligenceFindings.revisions.get | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### XML API

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-XML-001 | GET Service / list buckets | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-002 | DELETE Bucket | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-003 | GET Bucket / list objects | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-004 | GET Bucket / list multipart uploads | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-005 | GET Bucket ACL | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-006 | HEAD Bucket | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-007 | POST Bucket / multi-object delete | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-008 | PUT Bucket / create | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-009 | PUT Bucket ACL | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-010 | DELETE Object | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-011 | Abort multipart upload | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-012 | GET Object / download | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 8 |
| EXT-XML-013 | GET Object / list parts | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-014 | GET Object ACL | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-015 | GET Object encryption | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-016 | GET Object retention | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-017 | HEAD Object | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 5 |
| EXT-XML-018 | Initiate resumable upload | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-019 | Initiate XML multipart upload | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-020 | Complete XML multipart upload | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-021 | HTML form upload | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-022 | PUT Object / upload | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 4 |
| EXT-XML-023 | Upload XML multipart part | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-024 | Copy Object | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-025 | Compose Object | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-026 | PUT Object ACL | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-027 | PUT Object retention | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-028 | List HMAC keys | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-029 | Create HMAC key | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-030 | Update HMAC key | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-031 | Delete HMAC key | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-032 | Get bucket billing | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-033 | Get bucket cors | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-034 | Get bucket customPlacementConfig | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-035 | Get bucket encryptionConfig | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-036 | Get bucket lifecycle | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-037 | Get bucket location | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-038 | Get bucket logging | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-039 | Get bucket object-lock | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-040 | Get bucket storageClass | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-041 | Get bucket tagging | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-042 | Get bucket versioning | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-043 | Get bucket websiteConfig | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-044 | Set bucket billing | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-045 | Set bucket cors | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-046 | Set bucket encryptionConfig | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-047 | Set bucket lifecycle | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-048 | Set bucket logging | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-049 | Set bucket storageClass | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-050 | Set bucket tagging | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-051 | Set bucket versioning | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-052 | Set bucket websiteConfig | NOT_STARTED | P3 | EXACT | NONE | 0 |

### gRPC Storage v2

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| RPC-STORAGE-001 | DeleteBucket | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-002 | GetBucket | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-003 | CreateBucket | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-004 | ListBuckets | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-005 | LockBucketRetentionPolicy | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-006 | GetIamPolicy | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-007 | SetIamPolicy | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-008 | TestIamPermissions | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-009 | UpdateBucket | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-010 | ComposeObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-011 | DeleteObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-012 | RestoreObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-013 | CancelResumableWrite | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-014 | GetObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-015 | ReadObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-016 | BidiReadObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-017 | UpdateObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-018 | WriteObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-019 | BidiWriteObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-020 | ListObjects | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-021 | RewriteObject | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-022 | StartResumableWrite | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-023 | QueryWriteStatus | NOT_STARTED | P6 | EXACT | NONE | 0 |
| RPC-STORAGE-024 | MoveObject | NOT_STARTED | P6 | EXACT | NONE | 0 |

## behavior

### Access policy

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-017 | Local IAM deny-policy evaluation boundary | NOT_STARTED | P4 | LOCAL_SIMULATION | NONE | 0 |
| AUDIT-018 | Organization policy effects beyond public access | NOT_STARTED | P4 | LOCAL_SIMULATION | NONE | 0 |
| AUDIT-020 | Tag-derived soft-delete defaults | NOT_STARTED | P4 | LOCAL_SIMULATION | NONE | 0 |

### Authentication

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-002 | Explicit strict authentication profile | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 2 |
| DESIGN-004 | Anonymous versus authenticated callers | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 2 |
| DESIGN-005 | Service-account signing identity mapping | NOT_STARTED | P2 | LOCAL_SIMULATION | NONE | 0 |
| DESIGN-007 | Google OAuth token issuance boundary | NOT_STARTED | P2 | EXTERNAL | NONE | 0 |
| DESIGN-008 | Predefined storage roles permission expansion | IMPLEMENTED | P2 | EXACT | EXACT | 0 |
| DESIGN-010 | Unknown principal and revoked credential rejection | NOT_STARTED | P2 | EXACT | NONE | 0 |

### BUCKET

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-BUCKET-001 | Bucket identity and timestamps | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-BUCKET-002 | Labels | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-BUCKET-003 | Location and placement configuration | PARTIAL | P7 | LOCAL_SIMULATION | METADATA_ONLY | 0 |
| BEHAVIOR-BUCKET-004 | Storage classes | PARTIAL | P1 | EXACT | METADATA_ONLY | 0 |
| BEHAVIOR-BUCKET-005 | RPO and replication properties | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| BEHAVIOR-BUCKET-006 | Bucket default encryption key | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-007 | Encryption enforcement configuration | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-008 | IP filter configuration | NOT_STARTED | P2 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-009 | CORS configuration round-trip | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| BEHAVIOR-BUCKET-010 | Website configuration | NOT_STARTED | P2 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-011 | Usage/access logging configuration | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-012 | Default event-based hold | PARTIAL | P4 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-BUCKET-013 | Uniform access configuration | PARTIAL | P2 | EXACT | EXACT | 2 |
| BEHAVIOR-BUCKET-014 | Public access prevention | IMPLEMENTED | P2 | LOCAL_SIMULATION | LOCAL_SIMULATION | 2 |
| BEHAVIOR-BUCKET-015 | Soft delete policy | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-016 | Hard delete pause field | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-017 | Bucket soft-delete visibility and restore | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-018 | Hierarchical namespace configuration | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-019 | Autoclass configuration | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-020 | Autoclass state transitions | NOT_STARTED | P4 | EXACT | NONE | 0 |

### Batch semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-016 | Batch object holds | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-017 | Batch object deletion | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-018 | Batch metadata update | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-019 | Batch object rewrite | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-020 | Batch custom context update | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-021 | Batch object ACL update | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-022 | Batch source selection and dry run | NOT_STARTED | P7 | EXACT | NONE | 0 |

### CACHE

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-CACHE-001 | Anywhere/Rapid cache lifecycle | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| BEHAVIOR-CACHE-002 | Managed-folder Rapid cache policy | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### COMMON

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-COMMON-001 | Bucket and object name validation | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-002 | Literal object names and URI encoding | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-003 | JSON integer representation | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-004 | Partial response fields selectors | IMPLEMENTED | P1 | EXACT | EXACT | 1 |
| BEHAVIOR-COMMON-005 | Projection full/noAcl | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-006 | PUT replacement versus PATCH merge | PARTIAL | P1 | LOCAL_SIMULATION | LOCAL_SIMULATION | 1 |
| BEHAVIOR-COMMON-007 | HTTP and JSON error envelopes | IMPLEMENTED | P1 | EXACT | EXACT | 0 |
| BEHAVIOR-COMMON-008 | HTTP batch request format | NOT_STARTED | P3 | EXACT | NONE | 0 |
| BEHAVIOR-COMMON-009 | Strong data and listing consistency | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-010 | Authorization propagation model | NOT_STARTED | P2 | LOCAL_SIMULATION | NONE | 0 |
| BEHAVIOR-COMMON-011 | Generation/metageneration preconditions | IMPLEMENTED | P1 | EXACT | EXACT | 1 |
| BEHAVIOR-COMMON-012 | HTTP conditional requests | IMPLEMENTED | P1 | EXACT | EXACT | 1 |
| BEHAVIOR-COMMON-013 | Pagination and token validation | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-COMMON-014 | Project and billing context | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 0 |

### CONTEXT

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-CONTEXT-001 | Object contexts CRUD and validation | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-CONTEXT-002 | Context inheritance through operations | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-CONTEXT-003 | Extended context representation | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Control semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-006 | HNS folder atomic rename and hierarchy | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-BEH-007 | Managed folders and IAM inheritance | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-BEH-008 | Rapid Cache control state transitions | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-009 | Physical zonal Rapid Cache acceleration | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-010 | Storage Intelligence hierarchy and filters | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-011 | Long-running operation lifecycle | NOT_STARTED | P5 | EXACT | NONE | 0 |

### Cross-protocol semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-008 | Shared resource state across JSON XML and gRPC | NOT_STARTED | P6 | EXACT | NONE | 0 |

### Enrichment semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-023 | Object enrichment configuration filters | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-024 | Reserved enrichment context metadata | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-025 | Extended context payload retrieval | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-026 | Enrichment removal and rewrite interaction | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-027 | Face detection result fixtures | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-028 | Object detection result fixtures | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-029 | Image labeling result fixtures | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-030 | Content safety result fixtures | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-031 | PDF classification result fixtures | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### External integration

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-019 | Resource Manager tags and tag-binding boundary | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-032 | Pub/Sub event payload and outbox | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-033 | Google Pub/Sub delivery service | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-034 | Firebase Storage API and Security Rules | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-035 | Cloud Storage FUSE client compatibility | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-036 | Real geographic replication and durability | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-037 | Bucket relocation state machine | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-038 | Real Google KMS envelope encryption service | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-039 | Google OAuth and IAM principal issuance | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-040 | Storage Transfer Service and cross-bucket replication | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-041 | Google Cloud Monitoring and Logging backends | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-042 | VPC Service Controls and network perimeter | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-043 | IP filtering local decision model | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-044 | Cloud CDN, DNS and public HTTPS provisioning | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-045 | Billing and Requester Pays charge execution | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |

### FOLDER

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-FOLDER-001 | Flat prefix, managed folder, HNS distinction | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-FOLDER-002 | HNS recursive creation and deletion | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-FOLDER-003 | HNS folder rename transaction | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-FOLDER-004 | Managed-folder access inheritance | NOT_STARTED | P5 | EXACT | NONE | 0 |

### IAM enforcement

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-012 | allUsers anonymous grants | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-013 | allAuthenticatedUsers grants | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-014 | Bucket IAM permission enforcement | IMPLEMENTED | P2 | EXACT | EXACT | 2 |
| DESIGN-016 | Inherited and bucket policy evaluation | PARTIAL | P2 | EXACT | EXACT | 1 |
| DESIGN-017 | Object ACL enforcement with bucket IAM | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-018 | Default object ACL inheritance on creation | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-019 | Predefined ACL permission effects | PARTIAL | P2 | EXACT | EXACT | 0 |
| DESIGN-020 | UBLA disables ACL operations and grants | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-021 | UBLA 90-day irreversible transition | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-022 | PAP effective inherited/enforced state | IMPLEMENTED | P2 | LOCAL_SIMULATION | LOCAL_SIMULATION | 0 |
| DESIGN-023 | PAP denies public grants without deleting them | IMPLEMENTED | P2 | EXACT | EXACT | 2 |
| DESIGN-024 | PAP public-grant mutation errors | PARTIAL | P2 | EXACT | EXACT | 0 |
| DESIGN-025 | PAP permits valid signed access | IMPLEMENTED | P2 | EXACT | EXACT | 2 |
| DESIGN-026 | IAM policy version 3 conditions | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-027 | IAM policy etag concurrency | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-028 | testIamPermissions effective result | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 2 |
| DESIGN-029 | Signer permissions checked for signed requests | IMPLEMENTED | P2 | EXACT | EXACT | 2 |

### Insights semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-012 | Generate inventory report objects | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-013 | Storage Insights linked BigQuery datasets | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-014 | Public access insights fields | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-015 | Production advisor findings | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### LIFECYCLE

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-LIFECYCLE-001 | Lifecycle configuration validation | PARTIAL | P4 | EXACT | METADATA_ONLY | 2 |
| BEHAVIOR-LIFECYCLE-002 | Delete lifecycle action | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-003 | SetStorageClass lifecycle action | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-004 | AbortIncompleteMultipartUpload action | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-005 | Age and creation-date conditions | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-006 | Custom-time conditions | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-007 | Noncurrent-version conditions | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-008 | Name and storage-class conditions | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-009 | Object-size lifecycle conditions | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-010 | Deterministic scheduled evaluation | NOT_STARTED | P4 | LOCAL_SIMULATION | NONE | 0 |

### Limits and quotas

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-009 | Versioned API limits and optional quota model | NOT_STARTED | P1 | LOCAL_SIMULATION | NONE | 0 |

### OBJECT

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-OBJECT-001 | System and custom object metadata | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 3 |
| BEHAVIOR-OBJECT-002 | Generations and metagenerations | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| BEHAVIOR-OBJECT-003 | Object timestamps and lifecycle time | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-OBJECT-004 | Custom time monotonicity | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| BEHAVIOR-OBJECT-005 | Checksums | IMPLEMENTED | P1 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-006 | ETag semantics | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| BEHAVIOR-OBJECT-007 | Metadata mediaLink/selfLink | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-OBJECT-008 | Simple media upload | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-OBJECT-009 | Multipart JSON upload | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| BEHAVIOR-OBJECT-010 | Resumable initiation | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-011 | Resumable chunk writes | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-012 | Resumable status query | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-013 | Resumable cancellation and expiration | IMPLEMENTED | P3 | EXACT | EXACT | 0 |
| BEHAVIOR-OBJECT-014 | Resumable finalization visibility | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-015 | Read bytes and metadata separately | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-016 | Range reads | IMPLEMENTED | P1 | EXACT | EXACT | 0 |
| BEHAVIOR-OBJECT-017 | Content encoding and transcoding | NOT_STARTED | P3 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-018 | Prefix and delimiter listing | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-019 | Listing interval and glob filters | PARTIAL | P1 | EXACT | EXACT | 2 |
| BEHAVIOR-OBJECT-020 | Listing folders as prefixes | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-021 | Version and soft-delete listing | PARTIAL | P4 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-022 | Context-based object listing | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-023 | Object copy semantics | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-024 | Rewrite continuation sessions | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-025 | Composite object construction | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-026 | Composite component metadata | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-027 | Compose deleteSourceObjects | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-028 | Atomic object move | PARTIAL | P3 | EXACT | EXACT | 0 |
| BEHAVIOR-OBJECT-029 | Object versioning | PARTIAL | P4 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-030 | Soft delete and hard-delete deadline | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-031 | Restore object semantics | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-032 | HNS restore-token disambiguation | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-033 | Bulk restore filtering and counters | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-034 | Object retention configuration | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-035 | Temporary and event-based holds | PARTIAL | P4 | EXACT | LOCAL_SIMULATION | 3 |
| BEHAVIOR-OBJECT-036 | Bucket retention enforcement | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-037 | Customer-supplied encryption key checks | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-038 | Customer-managed encryption simulation | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### RAPID

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-RAPID-001 | RAPID zonal storage restrictions | NOT_STARTED | P6 | LOCAL_SIMULATION | NONE | 0 |
| BEHAVIOR-RAPID-002 | Appendable object creation | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RAPID-003 | Single-writer append handoff | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RAPID-004 | Append flush and finalization | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RAPID-005 | Read while appending | NOT_STARTED | P6 | EXACT | NONE | 0 |

### RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-RPC-001 | gRPC resource names and routing metadata | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RPC-002 | gRPC read/update field masks | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RPC-003 | gRPC streaming checksums and offsets | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RPC-004 | gRPC bidirectional ranged reads | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RPC-005 | gRPC canonical status and details | NOT_STARTED | P6 | EXACT | NONE | 0 |

### Signed access

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-030 | V4 RSA signed URL verification | IMPLEMENTED | P2 | EXACT | EXACT | 3 |
| DESIGN-031 | V4 HMAC signed URL verification | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-032 | Signed URL time and expiration bounds | IMPLEMENTED | P2 | EXACT | EXACT | 0 |
| DESIGN-033 | V4 canonical host and port | IMPLEMENTED | P2 | EXACT | EXACT | 5 |
| DESIGN-034 | V4 path encoding and Unicode | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-035 | V4 query sorting and encoding | PARTIAL | P2 | EXACT | EXACT | 3 |
| DESIGN-036 | V4 signed headers normalization | IMPLEMENTED | P2 | EXACT | EXACT | 3 |
| DESIGN-037 | V4 payload digest and UNSIGNED-PAYLOAD | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 2 |
| DESIGN-038 | Signed method and resource restrictions | IMPLEMENTED | P2 | EXACT | EXACT | 3 |
| DESIGN-039 | V4 Authorization header signatures | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-040 | Legacy V2 signed URLs | NOT_STARTED | P3 | EXACT | NONE | 0 |
| DESIGN-041 | Signed POST form policy validation | NOT_STARTED | P3 | EXACT | NONE | 0 |
| DESIGN-042 | Signed resumable initiation | NOT_STARTED | P3 | EXACT | NONE | 0 |
| DESIGN-043 | Resumable session capability semantics | NOT_STARTED | P3 | EXACT | NONE | 0 |
| DESIGN-044 | Signature failure wire errors | PARTIAL | P2 | EXACT | EXACT | 0 |

### Transport

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-010 | HTTP body framing and connection semantics | NOT_STARTED | P1 | EXACT | NONE | 0 |

### Transport semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-046 | MCP handshake and discovery | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-047 | Rapid Bucket protocol restrictions | NOT_STARTED | P7 | EXACT | NONE | 0 |

### URL and routing

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-047 | Path-style object endpoints | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-048 | Virtual-hosted-style object endpoints | IMPLEMENTED | P2 | EXACT | EXACT | 2 |
| DESIGN-049 | Media endpoint versus metadata endpoint | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-050 | Website host/index/error routing | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-051 | JSON API CORS defaults | IMPLEMENTED | P2 | EXACT | EXACT | 0 |
| DESIGN-052 | XML API bucket CORS enforcement | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-053 | Preflight does not grant object access | IMPLEMENTED | P2 | EXACT | EXACT | 0 |
| DESIGN-054 | Browser-auth download endpoint boundary | NOT_STARTED | P2 | EXTERNAL | NONE | 0 |

### XML semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-001 | XML pagination and markers | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-BEH-002 | XML namespace and error envelopes | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-BEH-003 | Resumable continuation, probing and cancellation | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-BEH-004 | Signed form policy validation | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-BEH-005 | Custom-domain website routing | NOT_STARTED | P3 | EXACT | NONE | 0 |

## project_requirement

### Authentication

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-001 | Explicit permissive authentication profile | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-003 | Local principal and test-token registry | IMPLEMENTED | P2 | PROJECT | PROJECT | 1 |
| DESIGN-006 | Local user and group membership fixtures | NOT_STARTED | P2 | PROJECT | NONE | 0 |
| DESIGN-009 | Custom roles through local configuration | NOT_STARTED | P2 | PROJECT | NONE | 0 |
| DESIGN-011 | Cross-project identity and ownership fixtures | NOT_STARTED | P2 | PROJECT | NONE | 0 |

### CLI compatibility

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-005 | gcloud storage command integration | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| AUDIT-006 | Legacy gsutil compatibility scope | NOT_STARTED | P7 | PROJECT | NONE | 0 |

### Container distribution

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DIST-003 | Verified multi-platform GHCR publication with immutable versions and attestations | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### Dependency freshness

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DEP-001 | Verified latest stable bootstrap | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-002 | Pinned current Rust and explicit edition/MSRV | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-003 | Latest direct libraries including major migrations | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-004 | Committed resolved lockfiles and locked builds | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-005 | Daily multi-ecosystem update detection | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DEP-006 | Start/release checks and weekly exception review | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DEP-007 | Explicit expiring incompatibility exceptions | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DEP-008 | Updated digest-pinned multiarch Docker inputs | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-009 | Pinned and current-stable CI lanes | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### Developer ergonomics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-058 | Docker single-command startup | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-059 | Native linux/amd64 container | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-060 | Native linux/arm64 container | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-061 | Host and compose-network connection examples | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-062 | Read-only fixture seeding | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DESIGN-063 | Persistent data volume | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DESIGN-064 | CLI and environment precedence | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-065 | HTTP and optional test TLS | IMPLEMENTED | P0 | PROJECT | PROJECT | 2 |
| DESIGN-066 | Non-root minimal runtime image | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-067 | Health, readiness and graceful shutdown | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DESIGN-068 | Isolated reset/reseed admin operations | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-069 | Deterministic clock and maintenance stepping | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### End-to-end onboarding

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| UX-001 | Clean-checkout source Docker build | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| UX-002 | Seed then ready startup | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| UX-003 | Host and peer-container CRUD flow | PARTIAL | P1 | PROJECT | PROJECT | 2 |
| UX-004 | Go and Ruby first-use examples | PARTIAL | P1 | PROJECT | PROJECT | 2 |
| UX-005 | Private/public authorization journey | IMPLEMENTED | P2 | PROJECT | PROJECT | 1 |
| UX-006 | Signed access and invalid-signature journey | IMPLEMENTED | P2 | PROJECT | PROJECT | 0 |
| UX-007 | Browser CORS and direct upload journey | IMPLEMENTED | P2 | PROJECT | PROJECT | 0 |
| UX-008 | Restart preserves data and permissions | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| UX-009 | Stop versus deliberate reset semantics | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| UX-010 | Isolated parallel CI runs | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| UX-011 | Resource-limited large-object workflow | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| UX-012 | Canonical English guide and troubleshooting verification | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### IAM enforcement

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-015 | Project/folder/organization inheritance fixtures | NOT_STARTED | P2 | PROJECT | NONE | 0 |

### Native distribution

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DIST-001 | Licensed native archives for macOS, Windows and Linux | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DIST-002 | Verified manual artifacts and immutable tag-driven GitHub Releases | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### Observability

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-015 | Bounded telemetry and credential redaction | NOT_STARTED | P1 | PROJECT | NONE | 0 |

### Project governance

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-021 | Deprecated notification API scope registry | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| AUDIT-022 | Behavioral release-note and restriction drift review | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-091 | Canonical machine-readable feature ledger | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-092 | Generated English matrix and optional Japanese companion | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-093 | Evidence-gated implemented status | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-094 | English primary contribution language | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-095 | Optional Japanese planning companion with shared feature IDs | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-096 | API schema/protobuf source drift tracking | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-097 | Separate method and behavior coverage | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-098 | Independent conformance and property tests | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-099 | Docker and generated-document CI gates | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### Resource efficiency

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-011 | Disconnected-client resource cleanup | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| AUDIT-012 | Disk exhaustion and write failure integrity | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| AUDIT-013 | File descriptor and active-session budgets | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| DESIGN-070 | Streaming bodies with bounded buffers | PARTIAL | P1 | PROJECT | PROJECT | 2 |
| DESIGN-071 | Bounded request and worker concurrency | PARTIAL | P1 | PROJECT | PROJECT | 2 |
| DESIGN-072 | Disk-backed metadata and indexed listing | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| DESIGN-073 | Immutable blobs and atomic catalog updates | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| DESIGN-074 | Cross-file crash recovery journal | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| DESIGN-075 | Opaque blob paths preserve object keys | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| DESIGN-076 | Single-process data-directory lock | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| DESIGN-077 | Bounded memory-backend mode | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| DESIGN-078 | Idle CPU without busy polling | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| DESIGN-079 | RSS and cgroup memory benchmarks | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| DESIGN-080 | CPU/latency/throughput benchmark matrix | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| DESIGN-081 | Fair fake-gcs-server comparison | NOT_STARTED | P1 | PROJECT | NONE | 0 |

### SDK compatibility

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-001 | Official Java SDK integration | NOT_STARTED | P7 | PROJECT | NONE | 0 |
| AUDIT-002 | Official .NET SDK integration | NOT_STARTED | P7 | PROJECT | NONE | 0 |
| AUDIT-003 | Official C++ SDK integration | NOT_STARTED | P7 | PROJECT | NONE | 0 |
| AUDIT-004 | Official PHP SDK integration | NOT_STARTED | P7 | PROJECT | NONE | 0 |
| AUDIT-007 | S3-compatible client subset validation | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| DESIGN-082 | Go official SDK integration | PARTIAL | P1 | PROJECT | PROJECT | 12 |
| DESIGN-083 | Ruby official SDK integration | PARTIAL | P1 | PROJECT | PROJECT | 4 |
| DESIGN-084 | Rails Active Storage workflows | PARTIAL | P2 | PROJECT | PROJECT | 1 |
| DESIGN-085 | Node official SDK integration | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| DESIGN-086 | Python official SDK integration | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| DESIGN-087 | Official Rust SDK integration | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| DESIGN-088 | Hermetic SDK tests deny unintended egress | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-089 | Optional Google differential conformance | NOT_STARTED | P0 | EXTERNAL | NONE | 0 |
| DESIGN-090 | Deterministic fault injection | NOT_STARTED | P0 | PROJECT | NONE | 0 |

### Signed access

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-045 | Local signing helper and fixture keys | IMPLEMENTED | P2 | PROJECT | PROJECT | 2 |

### Storage maintenance

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-014 | Catalog schema upgrade and compatibility policy | PARTIAL | P3 | PROJECT | PROJECT | 0 |
| AUDIT-016 | Safe blob reclamation during active reads | NOT_STARTED | P1 | PROJECT | NONE | 0 |

### URL and routing

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-046 | Public URL construction without permission mutation | IMPLEMENTED | P2 | PROJECT | PROJECT | 0 |
| DESIGN-055 | Listen versus advertised API/public addresses | PARTIAL | P2 | PROJECT | PROJECT | 2 |
| DESIGN-056 | Proxy-aware advertised URLs with explicit trust | NOT_STARTED | P2 | PROJECT | NONE | 0 |
| DESIGN-057 | Resumable Location reachable by clients | IMPLEMENTED | P2 | PROJECT | PROJECT | 1 |
