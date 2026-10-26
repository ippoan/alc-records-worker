//! 物品・携行品・伝達事項・指導記録の口 (Refs ippoan/rust-alc-api#680)。rust-alc-api の `crates/alc-misc` の
//! items.rs・carrying_items.rs・communication_items.rs・guidance_records.rs と repo/ の写しで、path・method・JSON の形・status は
//! rust と同じ (nuxt-items・alc-app・キオスクがそのまま使う)。
//!
//! - [`items`] — 物品 (フォルダの木・バーコード検索・持ち主の切り替え・種類の切り替え) と物品のファイル (`/item-files`)
//! - [`carrying`] — 携行品 (車両の条件つき)
//! - [`communication`] — 伝達事項 (一覧・有効なものだけの一覧)
//! - [`guidance`] — 指導記録 (3 階層の木) と添付
//!
//! 全部の口が `X-Tenant-ID` を要る (auth-worker が付ける。無い・UUID でない → 401。`alc_core_wasm::require_tenant_header`)。
//! rust と同じく role は見ない。
//! DB は `PgClient::tenant_tx` + `query_typed` 系だけ (名前付き prepared statement は使わない)。WHERE に tenant_id も書く
//! (rust は RLS 任せだった)。ログ・エラー応答は固定の語だけ (エラーの文・識別子・名前を載せない)。

pub mod carrying;
pub mod communication;
pub mod guidance;
pub mod items;
pub mod store;

use std::sync::Arc;

use alc_core_wasm::require_tenant_header;
use alc_worker_db::{kind, PgClient};
use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::{middleware, Router};
use futures_util::lock::Mutex;

use crate::store::{SharedStore, StoreError};

pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    pub pg: Arc<Mutex<PgClient>>,
    pub log: LogSink,
    /// 物品のファイル・指導記録の添付の置き場 (R2 の binding `RECORDS_R2`)。無ければファイルの読み書きは 503
    pub files: Option<SharedStore>,
}

impl AppState {
    /// DB の失敗を固定の語でログに出し、500 にする
    fn db_fail(&self, stage: &str, e: &tokio_postgres::Error) -> StatusCode {
        (self.log)(&format!("records: {stage} failed: db ({})", kind(e)));
        StatusCode::INTERNAL_SERVER_ERROR
    }

    /// R2 の失敗を固定の語でログに出し、500 にする
    fn store_fail(&self, stage: &str, e: StoreError) -> StatusCode {
        (self.log)(&format!("records: {stage} failed: r2 ({})", e.0));
        StatusCode::INTERNAL_SERVER_ERROR
    }

    /// R2 の binding (無ければ 503)
    fn store(&self, stage: &str) -> Result<SharedStore, StatusCode> {
        self.files.clone().ok_or_else(|| {
            (self.log)(&format!("records: {stage} failed: r2 (unconfigured)"));
            StatusCode::SERVICE_UNAVAILABLE
        })
    }
}

/// 頁 (rust と同じ: page は 1 以上 (既定 1)、per_page は既定 20・上限 100。下限は rust と同じく付けない)
pub fn paging(page: Option<i64>, per_page: Option<i64>) -> (i64, i64) {
    (page.unwrap_or(1).max(1), per_page.unwrap_or(20).min(100))
}

/// 本文の上限 (rust の main.rs の `DefaultBodyLimit` と同じ 20MB。ファイルの multipart に効く)
pub const BODY_LIMIT: usize = 20 * 1024 * 1024;

/// 全部の口。rust と同じく `/api` の下 (auth-worker は `/api/...` のまま Service Binding に渡す)
pub fn router() -> Router<AppState> {
    let api = Router::new()
        .merge(items::router())
        .merge(carrying::router())
        .merge(communication::router())
        .merge(guidance::router())
        .route_layer(middleware::from_fn(require_tenant_header));
    Router::new()
        .nest("/api", api)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

#[cfg(test)]
mod tests {
    use super::paging;

    #[test]
    fn paging_is_the_rust_rule() {
        assert_eq!(paging(None, None), (1, 20));
        assert_eq!(paging(Some(0), Some(500)), (1, 100));
        assert_eq!(paging(Some(3), Some(5)), (3, 5));
    }
}
