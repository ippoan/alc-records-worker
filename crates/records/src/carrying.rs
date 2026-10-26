//! 携行品 (rust の `alc-misc` の carrying_items.rs と repo/carrying_items.rs の写し。alc-app の管理画面とキオスクが呼ぶ)。
//!
//! - `GET /carrying-items` — 並び順・作成の順。各品に車両の条件 (`vehicle_conditions`、区分・値の順) を付ける
//! - `POST /carrying-items` (201) — `vehicle_conditions` を一緒に作る (同じ条件は 1 つに)
//! - `PUT /carrying-items/{id}` — 渡した列だけ変える。`vehicle_conditions` を渡せば全部置き換える。無ければ 404
//! - `DELETE /carrying-items/{id}` — 条件も消える (ON DELETE CASCADE)。無ければ 404
//!
//! 行の JSON は品の列に `vehicle_conditions` を足した形 (rust の `#[serde(flatten)]` と同じ)。
//! rust との違い: SQL に tenant_id を明示した (条件の表は品の id で絞り、品の tenant を確かめてから触る)。
//! 作成・更新は 1 つの transaction にした (rust は文ごとの接続で、途中で失敗すると品だけ残った)。

use alc_core_wasm::TenantId;
use alc_worker_db::{PgClient, TenantTx, TxOutput};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::Type;
use tokio_postgres::{Error, Row};
use uuid::Uuid;

use crate::AppState;

/// rust の `CarryingItem`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CarryingItem {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub item_name: String,
    pub is_required: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
}

/// rust の `CarryingItemVehicleCondition`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Condition {
    pub id: Uuid,
    pub carrying_item_id: Uuid,
    pub category: String,
    pub value: String,
}

/// rust の `CarryingItemWithConditions`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WithConditions {
    #[serde(flatten)]
    pub item: CarryingItem,
    pub vehicle_conditions: Vec<Condition>,
}

impl TxOutput for WithConditions {}

/// rust の `VehicleConditionInput`
#[derive(Debug, Clone, Deserialize)]
pub struct ConditionInput {
    pub category: String,
    pub value: String,
}

/// rust の `CreateCarryingItem`
#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub item_name: String,
    pub is_required: Option<bool>,
    pub sort_order: Option<i32>,
    #[serde(default)]
    pub vehicle_conditions: Vec<ConditionInput>,
}

/// rust の `UpdateCarryingItem`
#[derive(Debug, Deserialize)]
pub struct UpdateBody {
    pub item_name: Option<String>,
    pub is_required: Option<bool>,
    pub sort_order: Option<i32>,
    pub vehicle_conditions: Option<Vec<ConditionInput>>,
}

pub mod sql {
    pub const LIST: &str = "SELECT id, tenant_id, item_name, is_required, sort_order, created_at \
         FROM alc_api.carrying_items WHERE tenant_id = $1 ORDER BY sort_order, created_at";

    /// この tenant の品の条件の全部 (区分・値の順)
    pub const LIST_CONDITIONS: &str = "SELECT c.id, c.carrying_item_id, c.category, c.value \
         FROM alc_api.carrying_item_vehicle_conditions c \
         JOIN alc_api.carrying_items i ON i.id = c.carrying_item_id \
         WHERE i.tenant_id = $1 AND ($2::uuid IS NULL OR c.carrying_item_id = $2) \
         ORDER BY c.category, c.value";

    pub const INSERT: &str = "INSERT INTO alc_api.carrying_items (tenant_id, item_name, is_required, sort_order) \
         VALUES ($1, $2, $3, $4) RETURNING id, tenant_id, item_name, is_required, sort_order, created_at";

    pub const UPDATE: &str = "UPDATE alc_api.carrying_items SET \
         item_name = COALESCE($3, item_name), is_required = COALESCE($4, is_required), \
         sort_order = COALESCE($5, sort_order) \
         WHERE tenant_id = $1 AND id = $2 \
         RETURNING id, tenant_id, item_name, is_required, sort_order, created_at";

    pub const INSERT_CONDITION: &str = "INSERT INTO alc_api.carrying_item_vehicle_conditions \
         (carrying_item_id, category, value) VALUES ($1, $2, $3) \
         ON CONFLICT (carrying_item_id, category, value) DO NOTHING \
         RETURNING id, carrying_item_id, category, value";

    /// 品はこの transaction の中でこの tenant のものと確かめ済み (UPDATE ... RETURNING)
    pub const DELETE_CONDITIONS: &str =
        "DELETE FROM alc_api.carrying_item_vehicle_conditions WHERE carrying_item_id = $1";

    pub const DELETE: &str = "DELETE FROM alc_api.carrying_items WHERE tenant_id = $1 AND id = $2";
}

fn carrying_item(r: &Row) -> CarryingItem {
    CarryingItem {
        id: r.get(0),
        tenant_id: r.get(1),
        item_name: r.get(2),
        is_required: r.get(3),
        sort_order: r.get(4),
        created_at: r.get(5),
    }
}

fn condition(r: &Row) -> Condition {
    Condition {
        id: r.get(0),
        carrying_item_id: r.get(1),
        category: r.get(2),
        value: r.get(3),
    }
}

async fn conditions_of(
    tx: &TenantTx<'_>,
    tenant_id: Uuid,
    item_id: Option<Uuid>,
) -> Result<Vec<Condition>, Error> {
    let rows = tx
        .query_typed(
            sql::LIST_CONDITIONS,
            &[(&tenant_id, Type::UUID), (&item_id, Type::UUID)],
        )
        .await?;
    Ok(rows.iter().map(condition).collect())
}

/// 条件を足す (同じものは飛ばす = rust と同じく返す並びに入らない)
async fn insert_conditions(
    tx: &TenantTx<'_>,
    item_id: Uuid,
    inputs: &[ConditionInput],
) -> Result<Vec<Condition>, Error> {
    let mut out = Vec::new();
    for c in inputs {
        let row = tx
            .query_typed_opt(
                sql::INSERT_CONDITION,
                &[
                    (&item_id, Type::UUID),
                    (&c.category, Type::TEXT),
                    (&c.value, Type::TEXT),
                ],
            )
            .await?;
        out.extend(row.as_ref().map(condition));
    }
    Ok(out)
}

pub async fn list(pg: &mut PgClient, tenant_id: Uuid) -> Result<Vec<WithConditions>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let items = tx
                .query_typed(sql::LIST, &[(&tenant_id, Type::UUID)])
                .await?;
            let conds = conditions_of(tx, tenant_id, None).await?;
            Ok(items
                .iter()
                .map(|r| {
                    let item = carrying_item(r);
                    let vehicle_conditions = conds
                        .iter()
                        .filter(|c| c.carrying_item_id == item.id)
                        .cloned()
                        .collect();
                    WithConditions {
                        item,
                        vehicle_conditions,
                    }
                })
                .collect())
        })
    })
    .await
}

pub async fn create(
    pg: &mut PgClient,
    tenant_id: Uuid,
    b: CreateBody,
) -> Result<WithConditions, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let is_required = b.is_required.unwrap_or(true);
            let sort_order = b.sort_order.unwrap_or(0);
            let row = tx
                .query_typed_one(
                    sql::INSERT,
                    &[
                        (&tenant_id, Type::UUID),
                        (&b.item_name, Type::TEXT),
                        (&is_required, Type::BOOL),
                        (&sort_order, Type::INT4),
                    ],
                )
                .await?;
            let item = carrying_item(&row);
            let vehicle_conditions = insert_conditions(tx, item.id, &b.vehicle_conditions).await?;
            Ok(WithConditions {
                item,
                vehicle_conditions,
            })
        })
    })
    .await
}

pub async fn update(
    pg: &mut PgClient,
    tenant_id: Uuid,
    id: Uuid,
    b: UpdateBody,
) -> Result<Option<WithConditions>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let Some(row) = tx
                .query_typed_opt(
                    sql::UPDATE,
                    &[
                        (&tenant_id, Type::UUID),
                        (&id, Type::UUID),
                        (&b.item_name, Type::TEXT),
                        (&b.is_required, Type::BOOL),
                        (&b.sort_order, Type::INT4),
                    ],
                )
                .await?
            else {
                return Ok(None);
            };
            let vehicle_conditions = match &b.vehicle_conditions {
                Some(inputs) => {
                    tx.execute_typed(sql::DELETE_CONDITIONS, &[(&id, Type::UUID)])
                        .await?;
                    insert_conditions(tx, id, inputs).await?
                }
                None => conditions_of(tx, tenant_id, Some(id)).await?,
            };
            Ok(Some(WithConditions {
                item: carrying_item(&row),
                vehicle_conditions,
            }))
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
        .route("/carrying-items", get(list_items).post(create_item))
        .route("/carrying-items/{id}", put(update_item).delete(delete_item))
}

async fn list_items(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
) -> Result<Json<Vec<WithConditions>>, StatusCode> {
    let mut pg = state.pg.lock().await;
    list(&mut pg, tenant_id)
        .await
        .map(Json)
        .map_err(|e| state.db_fail("carrying list", &e))
}

async fn create_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<WithConditions>), StatusCode> {
    let mut pg = state.pg.lock().await;
    let it = create(&mut pg, tenant_id, b)
        .await
        .map_err(|e| state.db_fail("carrying create", &e))?;
    Ok((StatusCode::CREATED, Json(it)))
}

async fn update_item(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateBody>,
) -> Result<Json<WithConditions>, StatusCode> {
    let mut pg = state.pg.lock().await;
    update(&mut pg, tenant_id, id, b)
        .await
        .map_err(|e| state.db_fail("carrying update", &e))?
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
        .map_err(|e| state.db_fail("carrying delete", &e))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
