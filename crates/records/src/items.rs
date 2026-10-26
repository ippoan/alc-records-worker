//! 物品と物品のファイル (rust の `alc-misc` の items.rs と repo/items.rs の写し。nuxt-items が呼ぶ)。
//!
//! - `GET /items?parent_id=&owner_type=` — 1 つのフォルダの中 (`parent_id` 無しは最上位) を、持ち主の種類
//!   (`owner_type`、既定 `org`) で絞って、種類の逆順 (`item` → `folder`。rust と同じ `item_type DESC`)・名前の順
//! - `POST /items` (201)・`GET`/`PUT`/`DELETE /items/{id}` — 更新は渡した列だけ (COALESCE)。無ければ 404
//! - `GET /items/search?barcode=` — バーコードの一致を名前の順
//! - `POST /items/{id}/move` (`{parent_id}`)・`/ownership` (`{owner_type}`)・`/convert` (`{item_type}`) — 無ければ 404。
//!   フォルダを物品に変えるときは、中のものを 1 つ上のフォルダへ移す (`children_moved` に件数)
//! - `POST /item-files` — multipart の最初の 1 つを `items/{tenant}/{id}` に置き、行を返す (201)
//! - `GET /item-files/{id}` — 本体 (`Content-Type` は保存した値、`Content-Disposition` は inline・ASCII だけ。
//!   **rust と違う**、[`crate::store::content_disposition`])
//!
//! rust との違い: SQL に tenant_id を明示した (rust は RLS 任せ)。種類の切り替えは 1 つの transaction にした。

use alc_core_wasm::TenantId;
use alc_worker_db::{PgClient, TxOutput};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::{Error, Row};
use uuid::Uuid;

use crate::store::content_disposition;
use crate::AppState;

/// rust の `Item`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub owner_type: String,
    pub owner_user_id: Option<Uuid>,
    pub item_type: String,
    pub name: String,
    pub barcode: String,
    pub category: String,
    pub description: String,
    pub image_url: String,
    pub url: String,
    pub quantity: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TxOutput for Item {}

/// rust の `ItemFile`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemFile {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub created_at: DateTime<Utc>,
}

impl TxOutput for ItemFile {}

/// rust の `CreateItem`
#[derive(Debug, Deserialize)]
pub struct CreateItem {
    pub parent_id: Option<Uuid>,
    pub owner_type: Option<String>,
    pub owner_user_id: Option<Uuid>,
    pub item_type: Option<String>,
    pub name: String,
    pub barcode: Option<String>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub url: Option<String>,
    pub quantity: Option<i32>,
}

/// rust の `UpdateItem` (`None` の列は変えない)
#[derive(Debug, Deserialize)]
pub struct UpdateItem {
    pub name: Option<String>,
    pub barcode: Option<String>,
    pub category: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub url: Option<String>,
    pub quantity: Option<i32>,
}

macro_rules! columns {
    () => {
        "id, tenant_id, parent_id, owner_type, owner_user_id, item_type, name, barcode, category, \
         description, image_url, url, quantity, created_at, updated_at"
    };
}

pub mod sql {
    pub const LIST: &str = concat!(
        "SELECT ",
        columns!(),
        " FROM alc_api.items WHERE tenant_id = $1 AND parent_id IS NOT DISTINCT FROM $2 \
         AND owner_type = $3 ORDER BY item_type DESC, name"
    );

    pub const GET: &str = concat!(
        "SELECT ",
        columns!(),
        " FROM alc_api.items WHERE tenant_id = $1 AND id = $2"
    );

    pub const SEARCH: &str = concat!(
        "SELECT ",
        columns!(),
        " FROM alc_api.items WHERE tenant_id = $1 AND barcode = $2 ORDER BY name"
    );

    pub const INSERT: &str = concat!(
        "INSERT INTO alc_api.items \
         (tenant_id, parent_id, owner_type, owner_user_id, item_type, name, barcode, category, description, image_url, url, quantity) \
         VALUES ($1, $2, COALESCE($3, 'org'), $4, COALESCE($5, 'item'), $6, COALESCE($7, ''), COALESCE($8, ''), \
         COALESCE($9, ''), COALESCE($10, ''), COALESCE($11, ''), COALESCE($12, 1)) RETURNING ",
        columns!()
    );

    pub const UPDATE: &str = concat!(
        "UPDATE alc_api.items SET name = COALESCE($3, name), barcode = COALESCE($4, barcode), \
         category = COALESCE($5, category), description = COALESCE($6, description), \
         image_url = COALESCE($7, image_url), url = COALESCE($8, url), quantity = COALESCE($9, quantity), \
         updated_at = NOW() WHERE tenant_id = $1 AND id = $2 RETURNING ",
        columns!()
    );

    pub const DELETE: &str = "DELETE FROM alc_api.items WHERE tenant_id = $1 AND id = $2";

    pub const MOVE: &str = concat!(
        "UPDATE alc_api.items SET parent_id = $3, updated_at = NOW() WHERE tenant_id = $1 AND id = $2 RETURNING ",
        columns!()
    );

    pub const OWNERSHIP: &str = concat!(
        "UPDATE alc_api.items SET owner_type = $3, updated_at = NOW() WHERE tenant_id = $1 AND id = $2 RETURNING ",
        columns!()
    );

    pub const CONVERT: &str = concat!(
        "UPDATE alc_api.items SET item_type = $3, updated_at = NOW() WHERE tenant_id = $1 AND id = $2 RETURNING ",
        columns!()
    );

    pub const COUNT_CHILDREN: &str =
        "SELECT COUNT(*) FROM alc_api.items WHERE tenant_id = $1 AND parent_id = $2";

    /// 中のものを、この物品の親へ移す
    pub const MOVE_CHILDREN_UP: &str = "UPDATE alc_api.items SET \
         parent_id = (SELECT parent_id FROM alc_api.items WHERE tenant_id = $1 AND id = $2), updated_at = NOW() \
         WHERE tenant_id = $1 AND parent_id = $2";

    pub const INSERT_FILE: &str = "INSERT INTO alc_api.item_files (tenant_id, filename, content_type, size_bytes) \
         VALUES ($1, $2, $3, $4) RETURNING id, tenant_id, filename, content_type, size_bytes, created_at";

    pub const GET_FILE: &str =
        "SELECT id, tenant_id, filename, content_type, size_bytes, created_at \
         FROM alc_api.item_files WHERE tenant_id = $1 AND id = $2";
}

fn item(r: &Row) -> Item {
    Item {
        id: r.get(0),
        tenant_id: r.get(1),
        parent_id: r.get(2),
        owner_type: r.get(3),
        owner_user_id: r.get(4),
        item_type: r.get(5),
        name: r.get(6),
        barcode: r.get(7),
        category: r.get(8),
        description: r.get(9),
        image_url: r.get(10),
        url: r.get(11),
        quantity: r.get(12),
        created_at: r.get(13),
        updated_at: r.get(14),
    }
}

fn item_file(r: &Row) -> ItemFile {
    ItemFile {
        id: r.get(0),
        tenant_id: r.get(1),
        filename: r.get(2),
        content_type: r.get(3),
        size_bytes: r.get(4),
        created_at: r.get(5),
    }
}

/// R2 のキー (rust と同じ `items/{tenant}/{id}`)
pub fn file_key(tenant_id: Uuid, id: Uuid) -> String {
    format!("items/{tenant_id}/{id}")
}

/// `$1` = tenant に続く引数で、行の並びを返す文を流す
async fn rows(
    pg: &mut PgClient,
    tenant_id: Uuid,
    stmt: &'static str,
    args: Vec<(Box<dyn ToSql + Send + Sync>, Type)>,
) -> Result<Vec<Item>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let mut params: Vec<(&(dyn ToSql + Sync), Type)> = vec![(&tenant_id, Type::UUID)];
            params.extend(
                args.iter()
                    .map(|(v, t)| (v.as_ref() as &(dyn ToSql + Sync), t.clone())),
            );
            let rows = tx.query_typed(stmt, &params).await?;
            Ok(rows.iter().map(item).collect())
        })
    })
    .await
}

/// 1 行を返す文 (無ければ None)
async fn one(
    pg: &mut PgClient,
    tenant_id: Uuid,
    stmt: &'static str,
    args: Vec<(Box<dyn ToSql + Send + Sync>, Type)>,
) -> Result<Option<Item>, Error> {
    rows(pg, tenant_id, stmt, args)
        .await
        .map(|v| v.into_iter().next())
}

fn arg<T: ToSql + Send + Sync + 'static>(v: T, t: Type) -> (Box<dyn ToSql + Send + Sync>, Type) {
    (Box::new(v), t)
}

/// 種類の切り替え (フォルダ → 物品なら、中のものを 1 つ上へ移す)。1 つの transaction
pub async fn convert(
    pg: &mut PgClient,
    tenant_id: Uuid,
    id: Uuid,
    item_type: String,
) -> Result<Option<(Item, i64)>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let key = [
                (&tenant_id as &(dyn ToSql + Sync), Type::UUID),
                (&id, Type::UUID),
            ];
            let mut moved = 0;
            if item_type == "item" {
                moved = tx.query_typed_one(sql::COUNT_CHILDREN, &key).await?.get(0);
                if moved > 0 {
                    tx.execute_typed(sql::MOVE_CHILDREN_UP, &key).await?;
                }
            }
            let row = tx
                .query_typed_opt(
                    sql::CONVERT,
                    &[
                        (&tenant_id, Type::UUID),
                        (&id, Type::UUID),
                        (&item_type, Type::TEXT),
                    ],
                )
                .await?;
            Ok(row.map(|r| (item(&r), moved)))
        })
    })
    .await
}

pub async fn insert_file(
    pg: &mut PgClient,
    tenant_id: Uuid,
    filename: String,
    content_type: String,
    size_bytes: i64,
) -> Result<ItemFile, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let row = tx
                .query_typed_one(
                    sql::INSERT_FILE,
                    &[
                        (&tenant_id, Type::UUID),
                        (&filename, Type::TEXT),
                        (&content_type, Type::TEXT),
                        (&size_bytes, Type::INT8),
                    ],
                )
                .await?;
            Ok(item_file(&row))
        })
    })
    .await
}

pub async fn get_file(
    pg: &mut PgClient,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<Option<ItemFile>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let row = tx
                .query_typed_opt(
                    sql::GET_FILE,
                    &[(&tenant_id, Type::UUID), (&id, Type::UUID)],
                )
                .await?;
            Ok(row.as_ref().map(item_file))
        })
    })
    .await
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/items", get(list_items).post(create_item))
        .route("/items/search", get(search_items))
        .route(
            "/items/{id}",
            get(get_item).put(update_item).delete(delete_item),
        )
        .route("/items/{id}/move", post(move_item))
        .route("/items/{id}/ownership", post(change_ownership))
        .route("/items/{id}/convert", post(convert_type))
        .route("/item-files", post(upload_file))
        .route("/item-files/{id}", get(download_file))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub parent_id: Option<Uuid>,
    pub owner_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub barcode: String,
}

#[derive(Debug, Deserialize)]
pub struct MoveBody {
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct OwnershipBody {
    pub owner_type: String,
}

#[derive(Debug, Deserialize)]
pub struct ConvertBody {
    pub item_type: String,
}

/// rust の `ConvertResponse`
#[derive(Debug, Serialize)]
pub struct ConvertResponse {
    pub item: Item,
    pub children_moved: i64,
}

async fn list_items(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Item>>, StatusCode> {
    let owner_type = q.owner_type.unwrap_or_else(|| "org".to_owned());
    let mut pg = state.pg.lock().await;
    rows(
        &mut pg,
        tenant_id,
        sql::LIST,
        vec![arg(q.parent_id, Type::UUID), arg(owner_type, Type::TEXT)],
    )
    .await
    .map(Json)
    .map_err(|e| state.db_fail("items list", &e))
}

async fn search_items(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<Item>>, StatusCode> {
    let mut pg = state.pg.lock().await;
    rows(
        &mut pg,
        tenant_id,
        sql::SEARCH,
        vec![arg(q.barcode, Type::TEXT)],
    )
    .await
    .map(Json)
    .map_err(|e| state.db_fail("items search", &e))
}

async fn get_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Item>, StatusCode> {
    let mut pg = state.pg.lock().await;
    one(&mut pg, tenant_id, sql::GET, vec![arg(id, Type::UUID)])
        .await
        .map_err(|e| state.db_fail("items get", &e))?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn create_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Json(b): Json<CreateItem>,
) -> Result<(StatusCode, Json<Item>), StatusCode> {
    let mut pg = state.pg.lock().await;
    let created = one(
        &mut pg,
        tenant_id,
        sql::INSERT,
        vec![
            arg(b.parent_id, Type::UUID),
            arg(b.owner_type, Type::TEXT),
            arg(b.owner_user_id, Type::UUID),
            arg(b.item_type, Type::TEXT),
            arg(b.name, Type::TEXT),
            arg(b.barcode, Type::TEXT),
            arg(b.category, Type::TEXT),
            arg(b.description, Type::TEXT),
            arg(b.image_url, Type::TEXT),
            arg(b.url, Type::TEXT),
            arg(b.quantity, Type::INT4),
        ],
    )
    .await
    .map_err(|e| state.db_fail("items create", &e))?;
    // INSERT ... RETURNING は必ず 1 行
    Ok((
        StatusCode::CREATED,
        Json(created.ok_or(StatusCode::INTERNAL_SERVER_ERROR)?),
    ))
}

async fn update_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateItem>,
) -> Result<Json<Item>, StatusCode> {
    let mut pg = state.pg.lock().await;
    one(
        &mut pg,
        tenant_id,
        sql::UPDATE,
        vec![
            arg(id, Type::UUID),
            arg(b.name, Type::TEXT),
            arg(b.barcode, Type::TEXT),
            arg(b.category, Type::TEXT),
            arg(b.description, Type::TEXT),
            arg(b.image_url, Type::TEXT),
            arg(b.url, Type::TEXT),
            arg(b.quantity, Type::INT4),
        ],
    )
    .await
    .map_err(|e| state.db_fail("items update", &e))?
    .map(Json)
    .ok_or(StatusCode::NOT_FOUND)
}

async fn delete_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let mut pg = state.pg.lock().await;
    let deleted = pg
        .tenant_tx(tenant_id, move |tx| {
            Box::pin(async move {
                let n = tx
                    .execute_typed(sql::DELETE, &[(&tenant_id, Type::UUID), (&id, Type::UUID)])
                    .await?;
                Ok(n > 0)
            })
        })
        .await
        .map_err(|e| state.db_fail("items delete", &e))?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// 1 つの列を変える 3 口 (move / ownership) の共通の形
async fn change(
    state: AppState,
    tenant_id: Uuid,
    stmt: &'static str,
    id: Uuid,
    value: (Box<dyn ToSql + Send + Sync>, Type),
    stage: &str,
) -> Result<Json<Item>, StatusCode> {
    let mut pg = state.pg.lock().await;
    one(&mut pg, tenant_id, stmt, vec![arg(id, Type::UUID), value])
        .await
        .map_err(|e| state.db_fail(stage, &e))?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn move_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(b): Json<MoveBody>,
) -> Result<Json<Item>, StatusCode> {
    change(
        state,
        tenant_id,
        sql::MOVE,
        id,
        arg(b.parent_id, Type::UUID),
        "items move",
    )
    .await
}

async fn change_ownership(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(b): Json<OwnershipBody>,
) -> Result<Json<Item>, StatusCode> {
    change(
        state,
        tenant_id,
        sql::OWNERSHIP,
        id,
        arg(b.owner_type, Type::TEXT),
        "items ownership",
    )
    .await
}

async fn convert_type(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(b): Json<ConvertBody>,
) -> Result<Json<ConvertResponse>, StatusCode> {
    let mut pg = state.pg.lock().await;
    let (item, children_moved) = convert(&mut pg, tenant_id, id, b.item_type)
        .await
        .map_err(|e| state.db_fail("items convert", &e))?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(ConvertResponse {
        item,
        children_moved,
    }))
}

/// rust と同じ順: multipart の読み (400) → 行の作成 → R2 に置く (失敗は 500。行は残る = rust と同じ)。
/// R2 の binding が無ければ multipart を読む前に 503
async fn upload_file(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<ItemFile>), StatusCode> {
    let store = state.store("item-files upload")?;
    let field = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .ok_or(StatusCode::BAD_REQUEST)?;
    let filename = field.file_name().unwrap_or("file").to_owned();
    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_owned();
    let data = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
    let size_bytes = data.len() as i64;
    let file = {
        let mut pg = state.pg.lock().await;
        insert_file(
            &mut pg,
            tenant_id,
            filename,
            content_type.clone(),
            size_bytes,
        )
        .await
    }
    .map_err(|e| state.db_fail("item-files create", &e))?;
    store
        .put(&file_key(tenant_id, file.id), data.to_vec(), &content_type)
        .await
        .map_err(|e| state.store_fail("item-files upload", e))?;
    Ok((StatusCode::CREATED, Json(file)))
}

/// 行 (404) → R2 の binding (503) → R2 から読む
async fn download_file(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let file = {
        let mut pg = state.pg.lock().await;
        get_file(&mut pg, tenant_id, id).await
    }
    .map_err(|e| state.db_fail("item-files download", &e))?
    .ok_or(StatusCode::NOT_FOUND)?;
    let store = state.store("item-files download")?;
    let data = store
        .get(&file_key(tenant_id, id))
        .await
        .map_err(|e| state.store_fail("item-files download", e))?;
    Ok((
        [
            (header::CONTENT_TYPE, file.content_type),
            (
                header::CONTENT_DISPOSITION,
                content_disposition(&file.filename),
            ),
        ],
        data,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_key_is_the_rust_layout() {
        let t = Uuid::from_u128(1);
        let f = Uuid::from_u128(2);
        assert_eq!(file_key(t, f), format!("items/{t}/{f}"));
    }
}
