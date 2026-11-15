# Storage v2 の顧客指定の暗号鍵（CSEK）

<!-- markdownlint-configure-file {"MD013": false} -->

対象は BEHAVIOR-OBJECT-037（PARTIAL、LOCAL_SIMULATION）の gRPC 部分と、RPC-STORAGE-010〜012・014〜019・021〜023 の鍵のフィールド。
契約の正本は [英語版](../en/slices/grpc-v2-customer-supplied-keys.md)。HTTP 側の契約は [customer-supplied-keys.md](../en/slices/customer-supplied-keys.md) で、保存する鍵の指紋と鍵の検査は両 API で共通。オブジェクトのバイト列はローカルディスクに暗号化せずに保存する。

鍵は `common_object_request_params` に生バイト（Base64 ではない）で入れる。RewriteObject のソースの鍵は `copy_source_encryption_*` に入れる。空のフィールドは「指定なし」として扱う。
以下で「記録」とあるのは、2026-11-15 に実際の Cloud Storage へ gRPC で送って記録した差分ケース `grpc-csek` の結果。「ローカル判断」は gcs-mimic の決めごとで、ギャップ SPEC-GRPC-CSEK で追跡する。

- 検査の順序（記録）: 鍵のバイト列、アルゴリズム、指紋の順に調べる。指紋だけを送ると「鍵がない」エラーになり、JSON API のヘッダーの順序とは違う。
- エラー（記録）: 6種類とも `INVALID_ARGUMENT` で、`google.rpc.BadRequest` を1件付ける。違反の説明はメッセージと同じ文で、フィールド名は空。4種類の文言は JSON API と違う（`base64` が小文字、暗号化の有無のメッセージは "The target object is (not) encrypted by a customer-supplied encryption key."）。
- GetObject・UpdateObject（記録）: 鍵がなくても成功し、`checksums` を返さない（read mask で指定しても返さない）。正しい鍵なら返す。誤った鍵、未暗号化のオブジェクトへの鍵は失敗し、更新はコミットされない。
- ReadObject（記録）: 鍵がなければデータを送る前に失敗する。正しい鍵なら全体読み・範囲読みともデータ、`object_checksums`、`metadata.checksums` を返す。
- BidiReadObject（ローカル判断）: ReadObject と同じ規則を、開くたび、read handle で再開するたびに `read_object_spec` の鍵で適用する。handle は鍵を持たず、鍵の失敗で handle は無効にならない。
- ListObjects（記録）: 暗号化オブジェクトは `customer_encryption` 付き、`checksums` なしで並ぶ。
- 書き込み（記録）: WriteObject・BidiWriteObject は最初のメッセージの鍵で暗号化し、応答に `checksums` を返す。後続メッセージの鍵は、違う鍵でも無視する。StartResumableWrite の鍵はセッションに結び付き、`upload_id` のメッセージの鍵は無視する。暗号化済みのオブジェクトを鍵なしで上書きすると、未暗号化の世代になる。
- QueryWriteStatus（記録）: 鍵は誤っていても無視する。完了したセッションは鍵なしでも `checksums` と `customer_encryption` を返す。
- RewriteObject（記録）: 暗号化されたソースには鍵が要る。宛先の鍵だけで新しいオブジェクトの暗号化が決まるので、ソースの鍵だけなら復号、宛先の鍵だけなら暗号化、同じオブジェクトに両方なら鍵の交換になる。継続呼び出しで毎回両方の鍵を送る規則はローカル判断（JSON API と同じ）。
- ComposeObject（記録）: 1つの鍵で全ソースを復号し、結果を暗号化する。鍵なし、誤った鍵、未暗号化のソースが混ざる場合、未暗号化だけに鍵を付けた場合は失敗し、何も作らない。
- DeleteObject（記録）: 鍵が付いていても削除する。鍵は解析しない。
- RestoreObject（ローカル判断）と appendable の書き込みは、鍵を ErrorInfo 付きの UNIMPLEMENTED で拒否する。

## ローカルでの差分

鍵なしで始めた書き込みに後から鍵が付いた場合（単発ストリームの2通目、鍵なしセッションの `upload_id` のメッセージ）、Cloud Storage は `INTERNAL`（"We encountered an internal error. Please try again."）を返し、何も公開しない（記録）。
gcs-mimic は ResourceNotEncrypted（`INVALID_ARGUMENT`）を返し、何も公開しない。`INTERNAL` だとクライアントが成功しない要求を再試行してしまうため。再開可能なセッションは永続化済みのバイトを保ち、鍵なしで完了できる。
