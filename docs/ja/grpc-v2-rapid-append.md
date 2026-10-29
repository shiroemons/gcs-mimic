# Storage v2 Rapid appendable オブジェクト

<!-- markdownlint-configure-file {"MD013": false} -->

対象は BEHAVIOR-RAPID-002〜005（いずれも PARTIAL）、RPC-STORAGE-019（BidiWriteObject）と RPC-STORAGE-016（BidiReadObject）の appendable 部分、BEHAVIOR-RAPID-001 のローカル方針（NOT_STARTED のまま）。
契約の正本は [英語版](../en/slices/grpc-v2-rapid-append.md)。

`grpc_listen` を有効にし、Go の `STORAGE_EMULATOR_HOST_GRPC` をそのアドレスに指定する。
Go SDK v1.70.0 の `Writer.Append = true` で未確定（unfinalized）の appendable オブジェクトを作成でき、
`FinalizeOnClose` の既定は `false` なので Close 後もオブジェクトは追記可能なまま残る。
`NewWriterFromAppendableObject` で永続化済みのサイズから追記を再開（takeover）でき、
`FinalizeOnClose` を指定すると確定する。

- 作成: `WriteObjectSpec.appendable = true` で新しい世代を作る。世代は共有の採番から予約し、追記・確定で変わらない。metageneration は 1。
- flush: 受信したデータはすぐファイルに書き、`flush` で fsync してからサイズと CRC32C を確定する。flush されていないバイトは読み手に見えず、次の書き手が切り詰める。
- 応答: `flush`・`state_lookup`・正常な Close で `persisted_size`、`persisted_data_checksums`、`write_handle` を返す。作成中のストリームの最初の応答だけは未確定の `resource` を返し、クライアントが世代を知れるようにする。
- 読み取り: 確定済みオブジェクトがない場合、gRPC の GetObject・ReadObject・BidiReadObject は未確定の世代を選ぶ。読み取りは永続化済みサイズまでで、MD5 と `finalize_time` は返さない。DeleteObject で削除できる。
- takeover: `AppendObjectSpec` で現在の世代を開き直すと、最初のメッセージにすぐ応答し、新しい write handle を発行する。前の書き手は次の書き込みか flush で FAILED_PRECONDITION になる。古い handle では取り戻せない。
- 確定: `finish_write` で任意の CRC32C を検証し、予約済みの世代で共有の公開処理に流す。確定後は JSON・XML・ListObjects からも見える。MD5 の指定は INVALID_ARGUMENT。
- 永続化: 未確定状態は再起動後も残る。確定処理の途中で落ちた場合は、起動時の回復で公開済みなら行を削除し、未公開なら追記可能に戻す。

## RAPID の制約（BEHAVIOR-RAPID-001）

Google では appendable オブジェクトはゾーンバケット（ロケーションがゾーン、ストレージクラス Rapid、階層型名前空間と UBLA が必須）にだけ作れる。
gcs-mimic はゾーンの Rapid バケットを作れず、SDK のプローブは通常のバケットで `Writer.Append` を使うため、ローカルではどのバケットでも appendable オブジェクトを受け付け、ゾーンバケットの制約は適用しない。

## 制限

- 未確定オブジェクトは JSON・XML・ListObjects に出ず、バケット削除も妨げない。UpdateObject・コピー・rewrite・compose・move の対象にもならない。
- 既存オブジェクトを新しい appendable 世代で置き換える操作、appendable の `object_size`、CSEK は UNIMPLEMENTED。
- BidiReadObject は開いた時点の永続化済みサイズまでのスナップショットを読む。後の追記を読むには開き直す。
- 確定した appendable オブジェクトには共有の公開処理が計算した MD5 が付き、`finalize_time` は返さない。
