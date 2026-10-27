# 互換性の状態と未対応エラー

[英語の正本](../en/compatibility-policy.md)に従い、実装状態は
`compatibility/features.json` だけで管理する。機能表は同じ台帳から生成する。

認識している未対応の意味は、JSON API では 400 `badRequest`、XML API では
400 `InvalidArgument` で断る。既知の未実装 Google JSON ルートも同じ応答に揃える。
SDK が 501 をリトライして待ち続けたり、404 をリソースの不在と誤認したりすることを防ぐ。
エラーには未対応の指定・ルートと、指定を外すなどの対処例を示す。
Native と LegacyEmulator の両プロファイルで同じ方針を使う。

実装済みの操作でバケット・オブジェクトが存在しない場合は、従来の 404 `notFound` を保つ。
未知のパスも従来の扱いを保つ。既定値と同じ意味の入力を受け付ける既存の処理は変更しない。
これは未対応機能の実装や Google と同じ拒否応答を保証するものではない。

対象機能は DESIGN-082・DESIGN-083。広い SDK 対応範囲が残るため、両行は PARTIAL を維持する。
固定したルート一覧・受け入れケース・検証結果は
[スライス契約](../en/slices/sdk-unsupported-fail-fast.md)を参照する。
ローカルの選択は `compatibility/gaps.json` の SPEC-SDK-UNSUPPORTED に記録する。
