//! 伝達事項 (rust の `alc-misc` の communication_items.rs と repo/communication_items.rs の写し。alc-app が呼ぶ)。
//!
//! - `GET /communication-items?is_active=&target_employee_id=&page=&per_page=` — 優先度 (urgent → normal → ほか)・作成の
//!   新しい順の頁。`target_employee_id` を渡すと、その人宛てと全員宛て (NULL) だけ。応答は `{items, total, page, per_page}`
//! - `GET /communication-items/active?target_employee_id=` — 有効 (`is_active`) で、いまが有効期間の中のものだけ (遠隔点呼の画面用)
//! - `POST` (201)・`GET`/`PUT`/`DELETE /communication-items/{id}` — 更新は渡した列だけ。無ければ 404
//!
//! 一覧の行には宛先の社員の名前 (`target_employee_name`) を付ける。
//! rust との違い: SQL に tenant_id を明示した (社員の結び付けも同じ tenant に絞る)。

use alc_core_wasm::TenantId;
use alc_worker_db::{PgClient, TxOutput};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::Type;
use tokio_postgres::{Error, Row};
use uuid::Uuid;

use crate::{paging, AppState};

/// rust の `CommunicationItem`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub title: String,
    pub content: String,
    pub priority: String,
    pub target_employee_id: Option<Uuid>,
    pub is_active: bool,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_until: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TxOutput for Item {}

/// rust の `CommunicationItemWithName`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WithName {
    #[serde(flatten)]
    pub item: Item,
    pub target_employee_name: Option<String>,
}

impl TxOutput for WithName {}

/// rust の `CreateCommunicationItem`
#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub title: String,
    pub content: Option<String>,
    pub priority: Option<String>,
    pub target_employee_id: Option<Uuid>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_until: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
}

/// rust の `UpdateCommunicationItem` (宛先と有効期間は、渡したときだけ変える)
#[derive(Debug, Deserialize)]
pub struct UpdateBody {
    pub title: Option<String>,
    pub content: Option<String>,
    pub priority: Option<String>,
    pub target_employee_id: Option<Uuid>,
    pub is_active: Option<bool>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct Filter {
    pub is_active: Option<bool>,
    pub target_employee_id: Option<Uuid>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// rust の `CommunicationItemsResponse`
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub items: Vec<WithName>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

macro_rules! columns {
    ($p:literal) => {
        concat!(
            $p,
            "id, ",
            $p,
            "tenant_id, ",
            $p,
            "title, ",
            $p,
            "content, ",
            $p,
            "priority, ",
            $p,
            "target_employee_id, ",
            $p,
            "is_active, ",
            $p,
            "effective_from, ",
            $p,
            "effective_until, ",
            $p,
            "created_by, ",
            $p,
            "created_at, ",
            $p,
            "updated_at"
        )
    };
}

macro_rules! with_name {
    () => {
        concat!(
            "SELECT ",
            columns!("c."),
            ", e.name FROM alc_api.communication_items c \
             LEFT JOIN alc_api.employees e ON e.id = c.target_employee_id AND e.tenant_id = c.tenant_id \
             WHERE c.tenant_id = $1 "
        )
    };
}

macro_rules! order {
    () => {
        " ORDER BY CASE c.priority WHEN 'urgent' THEN 0 WHEN 'normal' THEN 1 ELSE 2 END, c.created_at DESC"
    };
}

pub mod sql {
    pub const COUNT: &str =
        "SELECT COUNT(*) FROM alc_api.communication_items c WHERE c.tenant_id = $1 \
         AND ($2::BOOLEAN IS NULL OR c.is_active = $2) \
         AND ($3::UUID IS NULL OR c.target_employee_id = $3 OR c.target_employee_id IS NULL)";

    pub const LIST: &str = concat!(
        with_name!(),
        "AND ($2::BOOLEAN IS NULL OR c.is_active = $2) \
         AND ($3::UUID IS NULL OR c.target_employee_id = $3 OR c.target_employee_id IS NULL)",
        order!(),
        " LIMIT $4 OFFSET $5"
    );

    pub const ACTIVE: &str = concat!(
        with_name!(),
        "AND c.is_active = true \
         AND (c.effective_from IS NULL OR c.effective_from <= now()) \
         AND (c.effective_until IS NULL OR c.effective_until >= now()) \
         AND ($2::UUID IS NULL OR c.target_employee_id = $2 OR c.target_employee_id IS NULL)",
        order!()
    );

    pub const GET: &str = concat!(
        "SELECT ",
        columns!(""),
        " FROM alc_api.communication_items WHERE tenant_id = $1 AND id = $2"
    );

    pub const INSERT: &str = concat!(
        "INSERT INTO alc_api.communication_items \
         (tenant_id, title, content, priority, target_employee_id, effective_from, effective_until, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING ",
        columns!("")
    );

    pub const UPDATE: &str = concat!(
        "UPDATE alc_api.communication_items SET \
         title = COALESCE($3, title), content = COALESCE($4, content), priority = COALESCE($5, priority), \
         target_employee_id = CASE WHEN $7 THEN $6 ELSE target_employee_id END, \
         is_active = COALESCE($8, is_active), \
         effective_from = CASE WHEN $10 THEN $9 ELSE effective_from END, \
         effective_until = CASE WHEN $12 THEN $11 ELSE effective_until END, \
         updated_at = now() WHERE tenant_id = $1 AND id = $2 RETURNING ",
        columns!("")
    );

    pub const DELETE: &str =
        "DELETE FROM alc_api.communication_items WHERE tenant_id = $1 AND id = $2";
}

fn item(r: &Row) -> Item {
    Item {
        id: r.get(0),
        tenant_id: r.get(1),
        title: r.get(2),
        content: r.get(3),
        priority: r.get(4),
        target_employee_id: r.get(5),
        is_active: r.get(6),
        effective_from: r.get(7),
        effective_until: r.get(8),
        created_by: r.get(9),
        created_at: r.get(10),
        updated_at: r.get(11),
    }
}

fn with_name(r: &Row) -> WithName {
    WithName {
        item: item(r),
        target_employee_name: r.get(12),
    }
}

pub async fn list(pg: &mut PgClient, tenant_id: Uuid, f: Filter) -> Result<ListResponse, Error> {
    let (page, per_page) = paging(f.page, f.per_page);
    let offset = (page - 1) * per_page;
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let base: [(&(dyn tokio_postgres::types::ToSql + Sync), Type); 3] = [
                (&tenant_id, Type::UUID),
                (&f.is_active, Type::BOOL),
                (&f.target_employee_id, Type::UUID),
            ];
            let total: i64 = tx.query_typed_one(sql::COUNT, &base).await?.get(0);
            let mut all = base.to_vec();
            all.push((&per_page, Type::INT8));
            all.push((&offset, Type::INT8));
            let rows = tx.query_typed(sql::LIST, &all).await?;
            Ok((total, rows.iter().map(with_name).collect::<Vec<_>>()))
        })
    })
    .await
    .map(|(total, items)| ListResponse {
        items,
        total,
        page,
        per_page,
    })
}

pub async fn active(
    pg: &mut PgClient,
    tenant_id: Uuid,
    target_employee_id: Option<Uuid>,
) -> Result<Vec<WithName>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let rows = tx
                .query_typed(
                    sql::ACTIVE,
                    &[(&tenant_id, Type::UUID), (&target_employee_id, Type::UUID)],
                )
                .await?;
            Ok(rows.iter().map(with_name).collect())
        })
    })
    .await
}

pub async fn get_one(pg: &mut PgClient, tenant_id: Uuid, id: Uuid) -> Result<Option<Item>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let row = tx
                .query_typed_opt(sql::GET, &[(&tenant_id, Type::UUID), (&id, Type::UUID)])
                .await?;
            Ok(row.as_ref().map(item))
        })
    })
    .await
}

pub async fn create(pg: &mut PgClient, tenant_id: Uuid, b: CreateBody) -> Result<Item, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let content = b.content.unwrap_or_default();
            let priority = b.priority.unwrap_or_else(|| "normal".to_owned());
            let row = tx
                .query_typed_one(
                    sql::INSERT,
                    &[
                        (&tenant_id, Type::UUID),
                        (&b.title, Type::TEXT),
                        (&content, Type::TEXT),
                        (&priority, Type::TEXT),
                        (&b.target_employee_id, Type::UUID),
                        (&b.effective_from, Type::TIMESTAMPTZ),
                        (&b.effective_until, Type::TIMESTAMPTZ),
                        (&b.created_by, Type::TEXT),
                    ],
                )
                .await?;
            Ok(item(&row))
        })
    })
    .await
}

pub async fn update(
    pg: &mut PgClient,
    tenant_id: Uuid,
    id: Uuid,
    b: UpdateBody,
) -> Result<Option<Item>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let (set_target, set_from, set_until) = (
                b.target_employee_id.is_some(),
                b.effective_from.is_some(),
                b.effective_until.is_some(),
            );
            let row = tx
                .query_typed_opt(
                    sql::UPDATE,
                    &[
                        (&tenant_id, Type::UUID),
                        (&id, Type::UUID),
                        (&b.title, Type::TEXT),
                        (&b.content, Type::TEXT),
                        (&b.priority, Type::TEXT),
                        (&b.target_employee_id, Type::UUID),
                        (&set_target, Type::BOOL),
                        (&b.is_active, Type::BOOL),
                        (&b.effective_from, Type::TIMESTAMPTZ),
                        (&set_from, Type::BOOL),
                        (&b.effective_until, Type::TIMESTAMPTZ),
                        (&set_until, Type::BOOL),
                    ],
                )
                .await?;
            Ok(row.as_ref().map(item))
        })
    })
    .await
}

pub async fn delete(pg: &mut PgClient, tenant_id: Uuid, id: Uuid) -> Result<bool, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let n = tx
                .execute_typed(sql::DELETE, &[(&tenant_id, Type::UUID), (&id, Type::UUID)])
                .await?;
            Ok(n > 0)
        })
    })
    .await
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/communication-items", get(list_items).post(create_item))
        .route("/communication-items/active", get(list_active))
        .route(
            "/communication-items/{id}",
            get(get_item).put(update_item).delete(delete_item),
        )
}

async fn list_items(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Query(f): Query<Filter>,
) -> Result<Json<ListResponse>, StatusCode> {
    let mut pg = state.pg.lock().await;
    list(&mut pg, tenant_id, f)
        .await
        .map(Json)
        .map_err(|e| state.db_fail("communication list", &e))
}

async fn list_active(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Query(f): Query<Filter>,
) -> Result<Json<Vec<WithName>>, StatusCode> {
    let mut pg = state.pg.lock().await;
    active(&mut pg, tenant_id, f.target_employee_id)
        .await
        .map(Json)
        .map_err(|e| state.db_fail("communication active", &e))
}

async fn get_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Item>, StatusCode> {
    let mut pg = state.pg.lock().await;
    get_one(&mut pg, tenant_id, id)
        .await
        .map_err(|e| state.db_fail("communication get", &e))?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn create_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<Item>), StatusCode> {
    let mut pg = state.pg.lock().await;
    let it = create(&mut pg, tenant_id, b)
        .await
        .map_err(|e| state.db_fail("communication create", &e))?;
    Ok((StatusCode::CREATED, Json(it)))
}

async fn update_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateBody>,
) -> Result<Json<Item>, StatusCode> {
    let mut pg = state.pg.lock().await;
    update(&mut pg, tenant_id, id, b)
        .await
        .map_err(|e| state.db_fail("communication update", &e))?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn delete_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let mut pg = state.pg.lock().await;
    if delete(&mut pg, tenant_id, id)
        .await
        .map_err(|e| state.db_fail("communication delete", &e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
