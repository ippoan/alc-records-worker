# alc-records-worker

物品・携行品・伝達事項・指導記録の Cloudflare Worker `alc-records`。rust-alc-api の `crates/alc-misc` の
items・carrying_items・communication_items・guidance_records から分割する (Refs ippoan/rust-alc-api#680)。型は ippoan/alc-carins-worker と同じ。

## 口

全部の口に `X-Tenant-ID` が要る (auth-worker が付ける。無い・UUID でない → 401)。path・method・JSON の形・status は rust-alc-api と同じ。

| path | method | 内容 |
|---|---|---|
| `/api/items` | GET / POST | 1 つのフォルダの中 (`parent_id`・`owner_type`、既定 `org`) の一覧と作成 (201) |
| `/api/items/search` | GET | `?barcode=` の一致 |
| `/api/items/{id}` | GET / PUT / DELETE | 取得・部分の更新・削除 |
| `/api/items/{id}/move`・`/ownership`・`/convert` | POST | フォルダの移動・持ち主の種類・物品とフォルダの切り替え (フォルダを物品にすると中を 1 つ上へ) |
| `/api/item-files` | POST | multipart の 1 つを R2 に置き、行を返す (201) |
| `/api/item-files/{id}` | GET | 本体 |
| `/api/carrying-items` | GET / POST | 携行品 (車両の条件つき) の一覧と作成 |
| `/api/carrying-items/{id}` | PUT / DELETE | 部分の更新 (条件を渡せば置き換え)・削除 |
| `/api/communication-items` | GET / POST | 伝達事項の一覧 (`is_active`・`target_employee_id`・頁) と作成 |
| `/api/communication-items/active` | GET | 有効で期間の中のものだけ |
| `/api/communication-items/{id}` | GET / PUT / DELETE | 取得・部分の更新・削除 |
| `/api/guidance-records` | GET / POST | 指導記録の木 (最上位の頁 + 3 階層まで + 添付) と作成 (親が無ければ 404、3 階層を超えれば 400) |
| `/api/guidance-records/{id}` | GET / PUT / DELETE | 取得・部分の更新・下の記録ごと削除 |
| `/api/guidance-records/{id}/attachments` | GET / POST | 添付の一覧と、multipart の 1 つを R2 に置く (201) |
| `/api/guidance-records/{id}/attachments/{att_id}` | GET / DELETE | 本体・削除 (R2 の実体は残す) |

**rust と違う** ところ:
- SQL に tenant_id を明示した (rust は RLS 任せ)。添付は、記録がその会社に無ければ R2 に書かずに 404 (rust は確かめなかった)
- 作成・更新の複数の文 (携行品と条件、物品の種類の切り替え、指導記録の深さの確かめ) を 1 つの transaction にした
- 添付の `storage_url` は `r2://alc-media/{key}` (rust は account ID 入りの公開 URL。どちらも読める)
- `Content-Disposition` は ASCII だけ (`inline; filename*=UTF-8''<percent-encoding>; filename="<ASCII の代替>"`)

## 構成

- 直下 = Worker (`src/lib.rs`・`src/db.rs`・`src/r2.rs`。wasm 専用)
- `crates/records/` = 口・SQL (native でも wasm でもビルドでき、テストと coverage の対象)
- R2: `RECORDS_R2` (本番 `alc-face-photos`・staging `alc-face-photos-staging`。rust の既定の共有 bucket)
- DB: 本番は Hyperdrive (`RECORDS_HYPERDRIVE`)、staging は Workers VPC (`RECORDS_DB_VPC`) の先の PgBouncer
- デプロイ: main への merge = staging、Tag Release (手動、`v*`) = 本番
