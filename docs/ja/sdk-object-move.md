# 同じバケット内で名前を変える

<!-- markdownlint-configure-file {"MD013": false} -->

通常のバケットでは、GoとRubyのmove APIでオブジェクト名を変更できます。
本文とメタデータを引き継ぎ、成功後は元の名前で取得できなくなります。
対象機能はJSON-OBJECTS-009とBEHAVIOR-OBJECT-028です。
両方とも対応範囲を限定したPARTIALで、未対応のケースはPENDINGです。

## SDKから呼ぶ

既存のバケットと、保存済みの`source.txt`を使います。
SDKの接続設定は[導入手順](container-usage.md)を参照してください。
追加のサーバー設定は不要です。

Go Storage v1.70.0では、移動元の`ObjectHandle`から呼びます。

```go
moved, err := bucket.Object("source.txt").Move(ctx,
    storage.MoveObjectDestination{Object: "destination.txt"})
```

Ruby Storage 1.62.1では、バケットから呼びます。

```ruby
moved = bucket.move_file("source.txt", "destination.txt")
```

成功すると、戻り値に移動先の情報が入ります。
本文、Content-Type、カスタムメタデータ、customTimeを引き継ぎます。
名前と世代、作成・更新時刻は変わり、メタ世代は1になります。
同名の移動先があれば上書きします。
元の削除と移動先の作成は一括で確定するため、失敗時は両方を保持します。

## 条件と失敗時の対処

移動元の世代を確認してから移動する場合は、Goの`If`を使います。

```go
moved, err := bucket.Object("source.txt").If(storage.Conditions{
    GenerationMatch: generation,
}).Move(ctx, storage.MoveObjectDestination{Object: "destination.txt"})
```

世代条件が合わない場合は412 `conditionNotMet`になります。
元と移動先を読み直し、最新の世代で再試行してください。
JSON APIでは元と移動先の世代・メタ世代のMatch／NotMatch条件を指定でき、
移動先の`ifGenerationMatch=0`は上書きを防ぎます。

元がない場合は404、同じ名前への移動は400になります。
元か上書き先にホールドがある場合は403になります。
ホールドを解除してから再試行してください。
strictモードでは元の取得・削除と、移動先の作成権限が必要です。
権限が不足する場合は、ローカルのIAM設定を確認してください。

## 対応範囲

バージョニング有効時は、上書き先の旧世代を残します。
移動する元のライブ世代は削除し、それ以前の旧世代は残します。
元の履歴の扱いはローカル仕様で、実際のGoogle Cloudとの比較は未実施です。

階層型名前空間、フォルダのrename、別バケットへのmoveは未対応です。
`projection=full`と`userProject`は400 `badRequest`になります。
`projection`を省略し、課金先の指定を外してください。
ACLや保持ポリシー、soft delete、move専用権限の検証は対象外です。

公式のAPI一覧とmove専用ページには、通常バケットの対象可否について記述の不一致が残っています。
[英語の契約・検証結果](../en/slices/sdk-object-move.md)と
[仕様差分SPEC-001](../../compatibility/gaps.json)を参照してください。
