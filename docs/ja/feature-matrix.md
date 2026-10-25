# Feature implementation matrix

Generated from `compatibility/features.json`; do not edit status here.

Research cutoff: 2026-10-23. Implementation status and achieved fidelity are independent.

API operations, behaviors and project requirements have separate denominators. Parameter and schema-field inventories are not additional features. SDK evidence is recorded separately on each row.

`Kind` groups rows by what they track: `api_operation` is an API method or operation, `behavior` is a semantic rule or workflow, and `project_requirement` is an emulator engineering or usability requirement.

## Implementation status

Status tracks completion of each row's declared acceptance cases.

| Kind | Rows | NOT_STARTED | IN_PROGRESS | PARTIAL | IMPLEMENTED | BLOCKED | NOT_APPLICABLE |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| api_operation | 298 | 278 | 0 | 19 | 1 | 0 | 0 |
| behavior | 201 | 131 | 0 | 32 | 38 | 0 | 0 |
| project_requirement | 92 | 36 | 0 | 27 | 29 | 0 | 0 |

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
| api_operation | 278 | 5 | 15 | 0 | 0 | 0 |
| behavior | 131 | 40 | 28 | 2 | 0 | 0 |
| project_requirement | 36 | 0 | 0 | 0 | 0 | 56 |

## api_operation

### Cloud Storage MCP

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-MCP-001 | MCP バケット一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-002 | MCP オブジェクト一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-003 | MCP バケット作成 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-004 | MCP バケット削除 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-005 | MCP テキスト取得（非推奨） | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-006 | MCP オブジェクト取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-007 | MCP テキスト書き込み | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-008 | MCP オブジェクト削除 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-MCP-009 | MCP メタデータ取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |

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
| JSON-BUCKETS-005 | buckets.insert | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| JSON-BUCKETS-006 | buckets.list | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| JSON-BUCKETS-007 | buckets.lockRetentionPolicy | NOT_STARTED | P4 | EXACT | NONE | 0 |
| JSON-BUCKETS-008 | buckets.patch | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 4 |
| JSON-BUCKETS-009 | buckets.relocate | NOT_STARTED | P7 | EXACT | NONE | 0 |
| JSON-BUCKETS-010 | buckets.restore | NOT_STARTED | P4 | EXACT | NONE | 0 |
| JSON-BUCKETS-011 | buckets.setIamPolicy | PARTIAL | P2 | EXACT | EXACT | 2 |
| JSON-BUCKETS-012 | buckets.testIamPermissions | NOT_STARTED | P2 | EXACT | NONE | 0 |
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
| JSON-OBJECTS-002 | objects.compose | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| JSON-OBJECTS-003 | objects.copy | PARTIAL | P3 | EXACT | EXACT | 1 |
| JSON-OBJECTS-004 | objects.delete | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 5 |
| JSON-OBJECTS-005 | objects.get | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 7 |
| JSON-OBJECTS-006 | objects.getIamPolicy | NOT_STARTED | P2 | EXACT | NONE | 0 |
| JSON-OBJECTS-007 | objects.insert | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 6 |
| JSON-OBJECTS-008 | objects.list | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 4 |
| JSON-OBJECTS-009 | objects.move | NOT_STARTED | P3 | EXACT | NONE | 0 |
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
| EXT-LOC-RPC-001 | Insights ロケーション一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-LOC-RPC-002 | Insights ロケーション取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-LOC-RPC-003 | Batch ロケーション一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-LOC-RPC-004 | Batch ロケーション取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Shared long-running RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-LRO-001 | オペレーションのキャンセル | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-LRO-002 | オペレーション結果の削除 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-LRO-003 | オペレーション取得 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-LRO-004 | オペレーション一覧 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-LRO-005 | オペレーション完了待ち | NOT_STARTED | P5 | EXACT | NONE | 0 |

### Storage Batch Operations

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BATCH-001 | Storage Batch locations get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-002 | Storage Batch locations list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-003 | Storage Batch jobs cancel | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-004 | Storage Batch jobs create | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-005 | Storage Batch jobs delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-006 | Storage Batch jobs get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-007 | Storage Batch jobs list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-008 | Storage Batch jobs.bucketOperations get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-009 | Storage Batch jobs.bucketOperations list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-010 | Storage Batch operations cancel | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-011 | Storage Batch operations delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-012 | Storage Batch operations get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-013 | Storage Batch operations list | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Batch Operations RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BATCH-RPC-001 | RPC ジョブ一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-002 | RPC ジョブ取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-003 | RPC ジョブ作成 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-004 | RPC ジョブ削除 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-005 | RPC ジョブ中止 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-006 | RPC バケット別処理一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BATCH-RPC-007 | RPC バケット別処理取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Control REST

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-023 | ViewObjectFullContextのv2 HTTPバインディング | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Control v2

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-CTRL-001 | Rapid Cache作成 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-002 | HNSフォルダ作成 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-003 | 管理フォルダ作成 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-004 | HNSフォルダ削除 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-005 | 管理フォルダ削除 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-006 | Rapid Cache無効化 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-007 | Rapid Cache取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-008 | HNSフォルダ取得 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-009 | フォルダ階層Intelligence設定取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-010 | 管理フォルダ取得 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-011 | 組織Intelligence設定取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-012 | プロジェクトIntelligence設定取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-013 | ストレージ配置情報取得 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-014 | Rapid Cache一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-015 | HNSフォルダ一覧 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-016 | 管理フォルダ一覧 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-017 | Rapid Cache一時停止 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-018 | HNSフォルダ名変更 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-019 | Rapid Cache再開 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-020 | Rapid Cache更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-021 | フォルダ階層Intelligence設定更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-022 | 組織Intelligence設定更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-023 | プロジェクトIntelligence設定更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-024 | HNSフォルダ再帰削除 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-025 | 管理フォルダ更新 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-026 | Rapid Cache作成 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-027 | Rapid Cache更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-028 | Rapid Cache無効化 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-029 | Rapid Cache取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-030 | Rapid Cache一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-CTRL-031 | IAMポリシー取得 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-032 | IAMポリシー設定 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-033 | IAM権限テスト | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-034 | Intelligence検出事項取得 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-035 | Intelligence検出事項一覧 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-036 | Intelligence検出事項集計 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-037 | Intelligence検出事項の指定履歴取得 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-038 | Intelligence検出事項の履歴一覧 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-CTRL-039 | オブジェクトの拡張コンテキスト取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Control v2 REST binding

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-CTRL-REST-001 | v2 REST HNSフォルダ作成 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-002 | v2 REST HNSフォルダ削除 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-003 | v2 REST HNSフォルダ取得 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-004 | v2 REST HNSフォルダ一覧 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-005 | v2 REST HNSフォルダ名変更 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-006 | v2 REST HNSフォルダ再帰削除 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-CTRL-REST-007 | v2 REST ストレージ配置情報取得 | NOT_STARTED | P5 | EXACT | NONE | 0 |

### Storage Insights

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-INS-001 | Storage Insights locations get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-002 | Storage Insights locations list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-003 | Storage Insights datasetConfigs create | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-004 | Storage Insights datasetConfigs delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-005 | Storage Insights datasetConfigs get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-006 | Storage Insights datasetConfigs linkDataset | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INS-007 | Storage Insights datasetConfigs list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-008 | Storage Insights datasetConfigs patch | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-009 | Storage Insights datasetConfigs unlinkDataset | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INS-010 | Storage Insights operations cancel | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-011 | Storage Insights operations delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-012 | Storage Insights operations get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-013 | Storage Insights operations list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-014 | Storage Insights reportConfigs create | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-015 | Storage Insights reportConfigs delete | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-016 | Storage Insights reportConfigs get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-017 | Storage Insights reportConfigs list | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-018 | Storage Insights reportConfigs patch | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-019 | Storage Insights reportConfigs.reportDetails get | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-020 | Storage Insights reportConfigs.reportDetails list | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Storage Insights RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-INS-RPC-001 | RPC レポート設定一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-002 | RPC レポート設定取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-003 | RPC レポート設定作成 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-004 | RPC レポート設定更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-005 | RPC レポート設定削除 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-006 | RPC レポート詳細一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-007 | RPC レポート詳細取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-008 | RPC データセット設定一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-009 | RPC データセット設定取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-010 | RPC データセット設定作成 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-011 | RPC データセット設定更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-012 | RPC データセット設定削除 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INS-RPC-013 | RPC BigQueryデータセットのリンク | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INS-RPC-014 | RPC BigQueryデータセットのリンク解除 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### Storage Intelligence

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-INT-001 | projects Intelligence設定 getIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-002 | projects Intelligence設定 updateIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-003 | folders Intelligence設定 getIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-004 | folders Intelligence設定 updateIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-005 | organizations Intelligence設定 getIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-006 | organizations Intelligence設定 updateIntelligenceConfig | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-INT-007 | 検出事項取得 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INT-008 | 検出事項一覧 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INT-009 | 検出事項集計 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INT-010 | 検出事項の履歴一覧 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-INT-011 | 検出事項の指定履歴取得 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### XML API

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-XML-001 | バケット一覧 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-002 | バケット削除 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-003 | オブジェクト一覧 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-004 | マルチパートセッション一覧 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-005 | バケットACL取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-006 | バケット存在確認 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-007 | 複数オブジェクト削除 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-008 | バケット作成 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-009 | バケットACL設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-010 | オブジェクト削除 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-011 | マルチパート中止 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-012 | オブジェクトダウンロード | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 8 |
| EXT-XML-013 | アップロード済みパート一覧 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-014 | オブジェクトACL取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-015 | オブジェクト暗号化情報取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-016 | オブジェクト保持設定取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-017 | オブジェクトメタデータ取得 | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 5 |
| EXT-XML-018 | 再開可能アップロード開始 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-019 | XMLマルチパート開始 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-020 | XMLマルチパート完了 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-021 | HTMLフォームアップロード | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-022 | オブジェクトアップロード | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 4 |
| EXT-XML-023 | XMLマルチパートのパート送信 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-024 | オブジェクトコピー | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-025 | オブジェクト合成 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-026 | オブジェクトACL設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-027 | オブジェクト保持設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-028 | HMAC鍵一覧 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-029 | HMAC鍵作成 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-030 | HMAC鍵更新 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-031 | HMAC鍵削除 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-032 | バケットRequester Pays取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-033 | バケットCORS取得 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-034 | バケット配置リージョン取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-035 | バケット暗号化設定取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-036 | バケットライフサイクル取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-037 | バケットロケーション取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-038 | バケットアクセスログ取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-039 | バケットオブジェクト保持の有効化状態取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-040 | バケットストレージクラス取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-041 | バケットラベル取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-042 | バケットバージョニング取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-043 | バケットウェブサイト設定取得 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-044 | バケットRequester Pays設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-045 | バケットCORS設定 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| EXT-XML-046 | バケット暗号化設定設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-047 | バケットライフサイクル設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-048 | バケットアクセスログ設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-049 | バケットストレージクラス設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-050 | バケットラベル設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-051 | バケットバージョニング設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-XML-052 | バケットウェブサイト設定設定 | NOT_STARTED | P3 | EXACT | NONE | 0 |

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
| AUDIT-017 | IAM Denyのローカル評価範囲 | NOT_STARTED | P4 | LOCAL_SIMULATION | NONE | 0 |
| AUDIT-018 | 公開設定以外の組織ポリシー効果 | NOT_STARTED | P4 | LOCAL_SIMULATION | NONE | 0 |
| AUDIT-020 | タグから決まるSoft delete既定値 | NOT_STARTED | P4 | LOCAL_SIMULATION | NONE | 0 |

### Authentication

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-002 | 認証・認可を検証する厳密モード | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 2 |
| DESIGN-004 | 匿名ユーザーと認証済みユーザーの区別 | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 2 |
| DESIGN-005 | サービスアカウント署名者の対応付け | NOT_STARTED | P2 | LOCAL_SIMULATION | NONE | 0 |
| DESIGN-007 | Google OAuthトークン発行との境界 | NOT_STARTED | P2 | EXTERNAL | NONE | 0 |
| DESIGN-008 | Storage定義済みロールの権限展開 | IMPLEMENTED | P2 | EXACT | EXACT | 0 |
| DESIGN-010 | 未知の主体・失効した認証情報の拒否 | NOT_STARTED | P2 | EXACT | NONE | 0 |

### BUCKET

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-BUCKET-001 | バケット識別子と時刻 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-BUCKET-002 | バケットラベル | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-BUCKET-003 | ロケーションと配置設定 | PARTIAL | P7 | LOCAL_SIMULATION | METADATA_ONLY | 0 |
| BEHAVIOR-BUCKET-004 | ストレージクラス | PARTIAL | P1 | EXACT | METADATA_ONLY | 0 |
| BEHAVIOR-BUCKET-005 | RPO・レプリケーション設定 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| BEHAVIOR-BUCKET-006 | バケット既定暗号鍵 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-007 | 暗号方式の強制設定 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-008 | IPフィルター設定 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-009 | CORS設定の保存・取得 | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| BEHAVIOR-BUCKET-010 | Webサイト設定 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-011 | アクセスログ設定 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-012 | 既定のイベントベース保留 | PARTIAL | P4 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-BUCKET-013 | 均一なバケットレベルアクセス設定 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-014 | 公開アクセス防止 | IMPLEMENTED | P2 | LOCAL_SIMULATION | LOCAL_SIMULATION | 2 |
| BEHAVIOR-BUCKET-015 | Soft deleteポリシー | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-016 | 恒久削除一時停止フィールド | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-017 | バケットのSoft deleteと復元 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-018 | 階層名前空間設定 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-019 | Autoclass設定 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-BUCKET-020 | Autoclassの状態遷移 | NOT_STARTED | P4 | EXACT | NONE | 0 |

### Batch semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-016 | バッチ保持設定変更 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-017 | バッチオブジェクト削除 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-018 | バッチメタデータ更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-019 | バッチオブジェクト再書き込み | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-020 | バッチカスタムコンテキスト更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-021 | バッチACL更新 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-022 | バッチ対象選択・ドライラン | NOT_STARTED | P7 | EXACT | NONE | 0 |

### CACHE

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-CACHE-001 | Anywhere・Rapid Cacheの状態遷移 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| BEHAVIOR-CACHE-002 | Managed folderのRapid Cacheポリシー | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### COMMON

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-COMMON-001 | バケット名・オブジェクト名の検証 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-002 | オブジェクト名とURIエンコード | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-003 | JSON整数の文字列表現 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-004 | fieldsによる部分レスポンス | IMPLEMENTED | P1 | EXACT | EXACT | 1 |
| BEHAVIOR-COMMON-005 | projectionのfull/noAcl | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-006 | PUT置換とPATCH部分更新 | PARTIAL | P1 | LOCAL_SIMULATION | LOCAL_SIMULATION | 1 |
| BEHAVIOR-COMMON-007 | HTTPステータスとJSONエラー | IMPLEMENTED | P1 | EXACT | EXACT | 0 |
| BEHAVIOR-COMMON-008 | HTTPバッチ形式 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| BEHAVIOR-COMMON-009 | データと一覧の強整合性 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-COMMON-010 | 認可変更の反映モデル | NOT_STARTED | P2 | LOCAL_SIMULATION | NONE | 0 |
| BEHAVIOR-COMMON-011 | 世代・メタ世代の前提条件 | IMPLEMENTED | P1 | EXACT | EXACT | 1 |
| BEHAVIOR-COMMON-012 | HTTP条件付き要求 | IMPLEMENTED | P1 | EXACT | EXACT | 1 |
| BEHAVIOR-COMMON-013 | ページングとトークン検証 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-COMMON-014 | プロジェクト・課金コンテキスト | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 0 |

### CONTEXT

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-CONTEXT-001 | Object contextsの更新・検証 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-CONTEXT-002 | 各操作でのcontexts継承 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-CONTEXT-003 | 拡張contexts表現 | NOT_STARTED | P7 | EXACT | NONE | 0 |

### Control semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-006 | HNSフォルダの原子的リネームと階層 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-BEH-007 | 管理フォルダとIAM継承 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| EXT-BEH-008 | Rapid Cacheの状態遷移 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-009 | 実際のゾーンキャッシュ性能 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-010 | Storage Intelligenceの階層・フィルタ | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-011 | 長時間オペレーションの状態遷移 | NOT_STARTED | P5 | EXACT | NONE | 0 |

### Cross-protocol semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-008 | JSON・XML・gRPC間の状態整合 | NOT_STARTED | P6 | EXACT | NONE | 0 |

### Enrichment semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-023 | Object enrichments対象設定 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-024 | 自動解析の予約コンテキスト | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-025 | 拡張コンテキスト取得 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-026 | 自動解析データの破棄・再書き込み | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-027 | 顔検出の結果フィクスチャ | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-028 | 物体検出の結果フィクスチャ | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-029 | 画像ラベルの結果フィクスチャ | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-030 | コンテンツ分類の結果フィクスチャ | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-031 | PDF分類の結果フィクスチャ | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### External integration

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-019 | Resource Managerタグとtag bindingの境界 | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-032 | Pub/Subイベントと永続送信キュー | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-033 | Google Pub/Subサービス | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-034 | Firebase Storage API・Security Rules | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-035 | Cloud Storage FUSEクライアント互換 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-036 | 実際の地理冗長化・耐久性 | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-037 | バケットロケーション変更の状態遷移 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-038 | Google KMSによる鍵管理 | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-039 | Google OAuth・IAM本人確認 | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-040 | Storage Transfer Service・バケット間レプリケーション | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-041 | Cloud Monitoring・Loggingバックエンド | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-042 | VPC Service Controls・ネットワーク境界 | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-043 | IPフィルタのローカル判定 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-044 | CDN・DNS・公開HTTPSの構築 | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-045 | 課金・Requester Paysの実課金 | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |

### FOLDER

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-FOLDER-001 | 3種類のフォルダー表現の区別 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-FOLDER-002 | HNSの再帰作成・削除 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-FOLDER-003 | HNSフォルダー名変更トランザクション | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-FOLDER-004 | Managed folderの認可継承 | NOT_STARTED | P5 | EXACT | NONE | 0 |

### IAM enforcement

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-012 | allUsersによる匿名アクセス許可 | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-013 | allAuthenticatedUsersによる認証済みユーザーへの許可 | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-014 | バケットIAMの実アクセス制御 | IMPLEMENTED | P2 | EXACT | EXACT | 2 |
| DESIGN-016 | 継承ポリシーとバケットポリシーの評価 | PARTIAL | P2 | EXACT | EXACT | 1 |
| DESIGN-017 | オブジェクトACLとバケットIAMの併用評価 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-018 | 作成時のデフォルトオブジェクトACL適用 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-019 | 定義済みACLの権限への反映 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-020 | UBLAによるACL操作・権限の無効化 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-021 | UBLAの90日経過後の解除不可制約 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-022 | 公開アクセス防止の継承・強制状態の評価 | IMPLEMENTED | P2 | LOCAL_SIMULATION | LOCAL_SIMULATION | 0 |
| DESIGN-023 | 公開アクセス防止による既存公開権限の抑止 | IMPLEMENTED | P2 | EXACT | EXACT | 2 |
| DESIGN-024 | 公開アクセス防止時の公開権限追加エラー | PARTIAL | P2 | EXACT | EXACT | 0 |
| DESIGN-025 | 公開アクセス防止下の有効な署名付きアクセス | IMPLEMENTED | P2 | EXACT | EXACT | 2 |
| DESIGN-026 | IAMポリシーversion 3と条件の評価 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-027 | IAMポリシーetagによる競合制御 | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-028 | testIamPermissionsの実効権限結果 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-029 | 署名付きリクエストに対する署名者権限の確認 | IMPLEMENTED | P2 | EXACT | EXACT | 2 |

### Insights semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-012 | インベントリレポート生成 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-013 | BigQuery連携データセット | NOT_STARTED | P7 | EXTERNAL | NONE | 0 |
| EXT-BEH-014 | 公開アクセス分析フィールド | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |
| EXT-BEH-015 | 本番の最適化提案・異常検出 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### LIFECYCLE

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-LIFECYCLE-001 | ライフサイクル設定検証 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-002 | ライフサイクルDelete | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-003 | ライフサイクルSetStorageClass | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-004 | 未完了multipartの自動中止 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-005 | 経過日数・作成日条件 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-006 | customTime条件 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-007 | 非現行世代条件 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-008 | 名前・ストレージクラス条件 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-009 | サイズ条件 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-LIFECYCLE-010 | 再現可能なライフサイクル評価 | NOT_STARTED | P4 | LOCAL_SIMULATION | NONE | 0 |

### Limits and quotas

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-009 | 版管理されたAPI制限と任意のクォータ再現 | NOT_STARTED | P1 | LOCAL_SIMULATION | NONE | 0 |

### OBJECT

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-OBJECT-001 | 標準・カスタムオブジェクトメタデータ | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 3 |
| BEHAVIOR-OBJECT-002 | 世代番号とメタ世代番号 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| BEHAVIOR-OBJECT-003 | オブジェクト時刻とライフサイクル時刻 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-OBJECT-004 | customTimeの更新制約 | NOT_STARTED | P1 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-005 | チェックサム | IMPLEMENTED | P1 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-006 | ETagの意味 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| BEHAVIOR-OBJECT-007 | mediaLink・selfLinkの生成 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-OBJECT-008 | 単純メディアアップロード | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 0 |
| BEHAVIOR-OBJECT-009 | JSON multipartアップロード | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 2 |
| BEHAVIOR-OBJECT-010 | 再開可能アップロード開始 | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-011 | 再開可能アップロードのチャンク送信 | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-012 | アップロード進捗照会 | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-013 | アップロード中止と期限切れ | IMPLEMENTED | P3 | EXACT | EXACT | 0 |
| BEHAVIOR-OBJECT-014 | アップロード確定時の可視性 | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-015 | データ取得とメタデータ取得 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-016 | 範囲取得 | IMPLEMENTED | P1 | EXACT | EXACT | 0 |
| BEHAVIOR-OBJECT-017 | コンテンツエンコードと変換 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-018 | prefix・delimiterによる一覧 | PARTIAL | P1 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-019 | 一覧の範囲・glob条件 | NOT_STARTED | P1 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-020 | フォルダーをprefixに含める一覧 | NOT_STARTED | P5 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-021 | 世代・Soft deleteの一覧 | PARTIAL | P4 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-022 | contextsによるオブジェクト一覧検索 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-023 | コピー時の意味論 | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-024 | rewriteの継続セッション | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-025 | オブジェクト合成 | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-026 | 合成オブジェクトのメタデータ | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-027 | compose時の元オブジェクト削除 | IMPLEMENTED | P3 | EXACT | EXACT | 1 |
| BEHAVIOR-OBJECT-028 | オブジェクトの原子的移動 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-029 | オブジェクトの世代管理 | PARTIAL | P4 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-030 | Soft deleteと恒久削除期限 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-031 | オブジェクト復元の意味論 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-032 | HNSのrestoreToken識別 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-033 | 一括復元の条件と件数 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-034 | オブジェクト保持設定 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-035 | 一時保留・イベントベース保留 | PARTIAL | P4 | EXACT | LOCAL_SIMULATION | 1 |
| BEHAVIOR-OBJECT-036 | バケット保持期間の強制 | NOT_STARTED | P4 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-037 | 顧客指定暗号鍵の検証 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| BEHAVIOR-OBJECT-038 | CMEKのローカル再現 | NOT_STARTED | P7 | LOCAL_SIMULATION | NONE | 0 |

### RAPID

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-RAPID-001 | RAPIDのゾーン構成と制約 | NOT_STARTED | P6 | LOCAL_SIMULATION | NONE | 0 |
| BEHAVIOR-RAPID-002 | 追記可能オブジェクト作成 | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RAPID-003 | 追記の単一writer制御 | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RAPID-004 | 追記flush・確定 | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RAPID-005 | 追記中の読み取り | NOT_STARTED | P6 | EXACT | NONE | 0 |

### RPC

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| BEHAVIOR-RPC-001 | gRPCのリソース名とルーティング | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RPC-002 | gRPCの読み取り・更新マスク | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RPC-003 | gRPCストリームのチェックサムとoffset | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RPC-004 | gRPC双方向の範囲読み取り | NOT_STARTED | P6 | EXACT | NONE | 0 |
| BEHAVIOR-RPC-005 | gRPCステータスと詳細 | NOT_STARTED | P6 | EXACT | NONE | 0 |

### Signed access

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-030 | V4 RSA署名付きURLの検証 | IMPLEMENTED | P2 | EXACT | EXACT | 3 |
| DESIGN-031 | V4 HMAC署名付きURLの検証 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-032 | 署名付きURLの日時・有効期限制約 | IMPLEMENTED | P2 | EXACT | EXACT | 0 |
| DESIGN-033 | V4の正規化対象Host・ポート | IMPLEMENTED | P2 | EXACT | EXACT | 5 |
| DESIGN-034 | V4のパス符号化・Unicode処理 | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-035 | V4クエリの並べ替え・符号化 | PARTIAL | P2 | EXACT | EXACT | 3 |
| DESIGN-036 | V4署名対象ヘッダーの正規化 | IMPLEMENTED | P2 | EXACT | EXACT | 3 |
| DESIGN-037 | V4のペイロードハッシュ・UNSIGNED-PAYLOAD | PARTIAL | P2 | EXACT | LOCAL_SIMULATION | 2 |
| DESIGN-038 | 署名対象メソッド・リソースの制限 | IMPLEMENTED | P2 | EXACT | EXACT | 3 |
| DESIGN-039 | V4 Authorizationヘッダー署名 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-040 | 従来のV2署名付きURL | NOT_STARTED | P3 | EXACT | NONE | 0 |
| DESIGN-041 | 署名付きPOSTフォームポリシーの検証 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| DESIGN-042 | 署名による再開可能アップロード開始 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| DESIGN-043 | 再開可能アップロードのセッションURI権限 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| DESIGN-044 | 署名失敗時のXMLエラー応答 | PARTIAL | P2 | EXACT | EXACT | 0 |

### Transport

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-010 | HTTPボディ形式と接続の意味論 | NOT_STARTED | P1 | EXACT | NONE | 0 |

### Transport semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-046 | MCP初期化・ツール一覧 | NOT_STARTED | P7 | EXACT | NONE | 0 |
| EXT-BEH-047 | Rapid BucketのAPI制約 | NOT_STARTED | P7 | EXACT | NONE | 0 |

### URL and routing

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-047 | パス形式のオブジェクトURL | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-048 | 仮想ホスト形式のオブジェクトURL | IMPLEMENTED | P2 | EXACT | EXACT | 2 |
| DESIGN-049 | メディア取得URLとメタデータURLの区別 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-050 | ウェブサイト用ホスト・index・エラーページの解決 | NOT_STARTED | P2 | EXACT | NONE | 0 |
| DESIGN-051 | JSON API固有のCORS既定動作 | IMPLEMENTED | P2 | EXACT | EXACT | 0 |
| DESIGN-052 | XML APIのバケットCORS設定反映 | IMPLEMENTED | P2 | EXACT | EXACT | 1 |
| DESIGN-053 | プリフライトとオブジェクト認可の独立 | IMPLEMENTED | P2 | EXACT | EXACT | 0 |
| DESIGN-054 | ブラウザ認証付きダウンロードURLとの境界 | NOT_STARTED | P2 | EXTERNAL | NONE | 0 |

### XML semantics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| EXT-BEH-001 | XML一覧のページング | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-BEH-002 | XML名前空間・エラー形式 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-BEH-003 | 再開可能アップロード継続・状態確認・中止 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-BEH-004 | 署名付きフォームのポリシー検証 | NOT_STARTED | P3 | EXACT | NONE | 0 |
| EXT-BEH-005 | 独自ドメインの静的サイト挙動 | NOT_STARTED | P3 | EXACT | NONE | 0 |

## project_requirement

### Authentication

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-001 | 認証を省略する開発モードの明示 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-003 | ローカル主体とテストトークンの登録 | IMPLEMENTED | P2 | PROJECT | PROJECT | 1 |
| DESIGN-006 | ユーザー・グループ所属のローカル定義 | NOT_STARTED | P2 | PROJECT | NONE | 0 |
| DESIGN-009 | カスタムロールのローカル設定 | NOT_STARTED | P2 | PROJECT | NONE | 0 |
| DESIGN-011 | プロジェクトをまたぐ主体・所有関係の再現 | NOT_STARTED | P2 | PROJECT | NONE | 0 |

### CLI compatibility

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-005 | gcloud storageコマンド互換 | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| AUDIT-006 | 既存gsutilの互換範囲 | NOT_STARTED | P7 | PROJECT | NONE | 0 |

### Container distribution

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DIST-003 | 検証・証跡付きのGHCR複数CPU公開と不変バージョン | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### Dependency freshness

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DEP-001 | 初期実装時の最新安定版確認 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-002 | 最新Rustの固定・editionとMSRVの明示 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-003 | 破壊的更新を含めた直接依存の最新化 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-004 | 解決済みlockfileのコミット・固定ビルド | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-005 | 複数エコシステムの毎日更新検出 | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DEP-006 | 作業開始・リリース前確認と毎週の例外見直し | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DEP-007 | 期限付きの非互換例外管理 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DEP-008 | 更新されるdigest固定の複数arch Docker入力 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DEP-009 | 固定版と最新stableのCI検証 | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### Developer ergonomics

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-058 | Dockerの単一コマンド起動 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-059 | linux/amd64コンテナの提供 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-060 | linux/arm64コンテナの提供 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-061 | ホスト・Compose内からの接続例 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-062 | 読み取り専用フィクスチャ投入 | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DESIGN-063 | 永続データボリューム | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DESIGN-064 | CLI・環境変数の優先順位 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-065 | HTTP・任意のテストTLS | IMPLEMENTED | P0 | PROJECT | PROJECT | 2 |
| DESIGN-066 | 非root・最小ランタイムイメージ | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-067 | ヘルス・準備完了確認・正常停止 | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DESIGN-068 | 分離された初期化・再投入管理操作 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-069 | 決定的な時計・メンテナンス実行 | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### End-to-end onboarding

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| UX-001 | クリーンチェックアウトからDockerビルド | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| UX-002 | 初期データ投入後の起動完了 | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| UX-003 | ホスト・別コンテナ双方からのCRUD | PARTIAL | P1 | PROJECT | PROJECT | 2 |
| UX-004 | Go・Rubyの初回利用例 | PARTIAL | P1 | PROJECT | PROJECT | 2 |
| UX-005 | 非公開・公開の権限制御フロー | IMPLEMENTED | P2 | PROJECT | PROJECT | 1 |
| UX-006 | 署名付きアクセスと不正署名の検証フロー | IMPLEMENTED | P2 | PROJECT | PROJECT | 0 |
| UX-007 | ブラウザCORS・直接アップロード | IMPLEMENTED | P2 | PROJECT | PROJECT | 0 |
| UX-008 | 再起動後のデータ・権限保持 | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| UX-009 | 停止と明示的データ初期化の区別 | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| UX-010 | 並列CI実行の分離 | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| UX-011 | 資源制限下の大容量オブジェクト操作 | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| UX-012 | 英語正本の導入ガイド・トラブルシューティング検証 | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### IAM enforcement

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-015 | プロジェクト・フォルダ・組織の継承設定 | NOT_STARTED | P2 | PROJECT | NONE | 0 |

### Native distribution

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DIST-001 | macOS・Windows・Linux向けのライセンス同梱バイナリ | PARTIAL | P0 | PROJECT | PROJECT | 0 |
| DIST-002 | 手動検証成果物と不変タグによるGitHub Release | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### Observability

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-015 | 上限付き観測情報と認証情報の秘匿 | NOT_STARTED | P1 | PROJECT | NONE | 0 |

### Project governance

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-021 | 非推奨通知APIの対応範囲台帳 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| AUDIT-022 | リリースノートによる動作・制約の差分確認 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-091 | 機械可読の単一機能台帳 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-092 | 英語正本表と任意の日本語補助表の生成 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-093 | 検証証跡を条件とする実装済み判定 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-094 | 英語を主言語とする開発運用 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-095 | 共通の機能IDを使う任意の日本語計画資料 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-096 | APIスキーマ・protoの仕様差分追跡 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-097 | メソッド網羅率と動作網羅率の分離 | IMPLEMENTED | P0 | PROJECT | PROJECT | 0 |
| DESIGN-098 | 独立した適合試験・性質ベース試験 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-099 | Docker・生成ドキュメントのCI検査 | PARTIAL | P0 | PROJECT | PROJECT | 0 |

### Resource efficiency

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-011 | 切断クライアントの資源解放 | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| AUDIT-012 | ディスク枯渇・書込失敗時の整合性 | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| AUDIT-013 | ファイル記述子・稼働セッションの上限 | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| DESIGN-070 | 固定上限バッファによる本体ストリーミング | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| DESIGN-071 | リクエスト・ワーカー同時実行数の上限 | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| DESIGN-072 | ディスク上メタデータ・索引付き一覧取得 | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| DESIGN-073 | 不変blobとカタログの原子的更新 | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| DESIGN-074 | 複数ファイルにまたがるクラッシュ回復 | PARTIAL | P1 | PROJECT | PROJECT | 0 |
| DESIGN-075 | 不透明なblob保存パスとキーの完全保持 | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| DESIGN-076 | データディレクトリの単一プロセス制約 | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| DESIGN-077 | 上限付きインメモリ動作 | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| DESIGN-078 | ビジーループを避けたアイドルCPU低減 | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| DESIGN-079 | RSS・cgroupメモリ測定 | IMPLEMENTED | P1 | PROJECT | PROJECT | 0 |
| DESIGN-080 | CPU・レイテンシ・スループット測定表 | NOT_STARTED | P1 | PROJECT | NONE | 0 |
| DESIGN-081 | fake-gcs-serverとの同条件比較 | NOT_STARTED | P1 | PROJECT | NONE | 0 |

### SDK compatibility

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-001 | 公式Java SDK互換 | NOT_STARTED | P7 | PROJECT | NONE | 0 |
| AUDIT-002 | 公式.NET SDK互換 | NOT_STARTED | P7 | PROJECT | NONE | 0 |
| AUDIT-003 | 公式C++ SDK互換 | NOT_STARTED | P7 | PROJECT | NONE | 0 |
| AUDIT-004 | 公式PHP SDK互換 | NOT_STARTED | P7 | PROJECT | NONE | 0 |
| AUDIT-007 | S3互換クライアントの対応範囲検証 | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| DESIGN-082 | Go公式SDKとの統合検証 | PARTIAL | P1 | PROJECT | PROJECT | 10 |
| DESIGN-083 | Ruby公式SDKとの統合検証 | PARTIAL | P1 | PROJECT | PROJECT | 1 |
| DESIGN-084 | Rails Active Storageの一連の操作 | NOT_STARTED | P2 | PROJECT | NONE | 0 |
| DESIGN-085 | Node公式SDKとの統合検証 | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| DESIGN-086 | Python公式SDKとの統合検証 | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| DESIGN-087 | Rust公式SDKとの統合検証 | NOT_STARTED | P3 | PROJECT | NONE | 0 |
| DESIGN-088 | SDK試験の意図しない外部通信防止 | NOT_STARTED | P0 | PROJECT | NONE | 0 |
| DESIGN-089 | 任意の実GCS差分適合試験 | NOT_STARTED | P0 | EXTERNAL | NONE | 0 |
| DESIGN-090 | 決定的な障害注入 | NOT_STARTED | P0 | PROJECT | NONE | 0 |

### Signed access

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-045 | ローカル署名ヘルパーとテスト鍵 | IMPLEMENTED | P2 | PROJECT | PROJECT | 2 |

### Storage maintenance

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| AUDIT-014 | カタログ形式の更新と互換方針 | PARTIAL | P3 | PROJECT | PROJECT | 0 |
| AUDIT-016 | 読み取り中のblobの安全な回収 | NOT_STARTED | P1 | PROJECT | NONE | 0 |

### URL and routing

| ID | Feature | Status | Phase | Target fidelity | Achieved fidelity | SDK records |
| --- | --- | --- | --- | --- | --- | ---: |
| DESIGN-046 | 権限を変えない公開URLの組み立て | IMPLEMENTED | P2 | PROJECT | PROJECT | 0 |
| DESIGN-055 | 待受・API公開先・オブジェクト公開先の分離 | PARTIAL | P2 | PROJECT | PROJECT | 2 |
| DESIGN-056 | 明示的なプロキシ信頼設定とURL生成 | NOT_STARTED | P2 | PROJECT | NONE | 0 |
| DESIGN-057 | クライアントから到達できる再開用Location | IMPLEMENTED | P2 | PROJECT | PROJECT | 1 |
