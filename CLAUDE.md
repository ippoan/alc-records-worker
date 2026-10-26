# alc-records-worker

物品・携行品・伝達事項・指導記録の Worker `alc-records` (Refs ippoan/rust-alc-api#680)。rust-alc-api の `crates/alc-misc` の
items・carrying_items・communication_items・guidance_records を移す先で、path・method・JSON の形・status は rust と同じ
(nuxt-items・alc-app・キオスクがそのまま使う)。口は `/api/` の下: `items`・`item-files` (`crates/records/src/items.rs`)・
`carrying-items` (`carrying.rs`)・`communication-items` (`communication.rs`)・`guidance-records` と添付 (`guidance.rs`)。
直下 = Worker (wasm 専用)、`crates/records/` = 口・SQL。

**呼び手は auth-worker の Service Binding だけ** (auth-worker が JWT を検証し `X-Tenant-ID` を付け直す。この Worker は
認証しない)。auth-worker の振り分け (`ALC_BINDING_ROUTES`) は別の PR。

## コマンド
```bash
bash scripts/check-exposure.sh && bash scripts/check-exposure-test.sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --release -- -D warnings
bash scripts/fetch-migrations.sh   # 版は scripts/ALC_MIGRATIONS_REV
RECORDS_TEST_ADMIN_DATABASE_URL=postgresql://postgres:<pw>@127.0.0.1:<port>/postgres cargo test --workspace
worker-build --release && npx wrangler@4.144.0 deploy --dry-run
```
DB テストは使い捨ての postgres:17 へ。URL が無ければ失敗 (skip しない)。接続ロールは `alc_api_rt`。

## 規範
- 公開範囲を弱めない: 本番・staging とも Service Binding だけ (どの env も `workers_dev`/`preview_urls` = false、route 無し)。
  Hyperdrive (`RECORDS_HYPERDRIVE`) は本番だけ。`wrangler.toml` の表の順を変えない (新しい表は placement より後)。
- **ログ・エラー応答は固定の語だけ。** エラーの文 (`{e}`)・tenant_id・名前・ファイル名を出さない。
  DB の失敗は `records: <段> failed: db (<kind>)`、R2 は `records: <段> failed: r2 (<段の名前>)`。応答の本文は rust と同じく空。
- R2 は `RECORDS_R2` 1 つ (rust の既定の共有 bucket に相乗り。物品のファイル `items/{tenant}/{id}`、指導記録の添付
  `{tenant}/guidance/{record}/{uuid}.{ext}`)。置き場は `store::FileStore` (Worker は `src/r2.rs`、テストはモック)。
  R2 の await を `tenant_tx` の中に入れない。staging は本番の bucket を指さない (check-exposure.sh)。
  添付の `storage_url` は `r2://alc-media/{key}` (rust は account ID 入りの公開 URL)。読むときは URL の中の
  `/{tenant}/guidance/{record}/` から後ろをキーにする (`guidance::extract_key`) ので rust の行も読める。
- role は見ない (rust も `X-Tenant-ID` だけ)。
- DB は `PgClient::tenant_tx` + `query_typed` 系だけ。名前付き prepared statement 禁止。Row を tx の外へ出さない。
  WHERE に tenant_id も明示する (tenant の列の無い表は親の行で絞る)。SQL の意味は rust の repo と同じにする。
- `coverage_100.toml` の登録を外さない。CI の本数 (lib 5・items_db 4・records_db 4) はテストを足したら上げる。
- public repo: ホスト名・tenant UUID・個人情報を書かない。`v*` タグ = 本番 (Tag Release 手動)。
- kit の rev は直下 `Cargo.toml` の 1 か所 (alc-carins-worker と同じ rev)。
