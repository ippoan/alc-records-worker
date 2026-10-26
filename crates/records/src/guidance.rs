//! 指導記録と添付 (rust の `alc-misc` の guidance_records.rs と repo/guidance_records.rs の写し。alc-app が呼ぶ)。
//!
//! - `GET /guidance-records?employee_id=&guidance_type=&date_from=&date_to=&page=&per_page=` — 最上位の記録を指導日の新しい順の頁で
//!   取り、その下の記録 (3 階層まで) と添付を木にして返す。応答は `{records, total, page, per_page}` (`total` は最上位の数)
//! - `POST /guidance-records` (201) — 親を渡すと 1 つ深くなる。親が無ければ 404、親が 3 階層目なら 400
//! - `GET`/`PUT /guidance-records/{id}` — 更新は渡した列だけ。無ければ 404
//! - `DELETE /guidance-records/{id}` — 下の記録も一緒に消す (204 / 404)
//! - `GET`/`POST /guidance-records/{id}/attachments` — 添付の一覧 (作成の順) と、multipart の最初の 1 つを
//!   `{tenant}/guidance/{record}/{uuid}.{ext}` に置いて行を作る (201)
//! - `GET`/`DELETE /guidance-records/{id}/attachments/{att_id}` — 本体 (inline・ASCII だけの `Content-Disposition`) と削除
//!   (R2 の実体は消さない = rust と同じ)
//!
//! rust との違い:
//! - SQL に tenant_id を明示した (添付の表は tenant の列を持たないので、記録の tenant で絞る)
//! - 添付は、記録がこの会社に無ければ (無い・別の会社) R2 に書かずに 404 (rust は確かめなかった)
//! - 添付の `storage_url` は rust と違い `r2://alc-media/{key}` を保存する (rust の接頭辞は account ID 入りで public repo に
//!   書けない)。読むときは URL の中の `/{tenant_id}/guidance/{record_id}/` から後ろをキーにする ([`extract_key`]) ので、
//!   rust が保存した URL もそのまま読める
//! - 新しい添付の uuid は `gen_random_uuid()` で取る (wasm で乱数の crate を持たないため)

use alc_core_wasm::TenantId;
use alc_worker_db::{PgClient, TenantTx, TxOutput};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{ToSql, Type};
use tokio_postgres::{Error, Row};
use uuid::Uuid;

use crate::store::content_disposition;
use crate::{paging, AppState};

/// 添付の `storage_url` の接頭辞 (読み手はここを見ない。モジュールの注)
pub const URL_PREFIX: &str = "r2://alc-media/";

/// rust の `GuidanceRecord`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Record {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub employee_id: Uuid,
    pub guidance_type: String,
    pub title: String,
    pub content: String,
    pub guided_by: Option<String>,
    pub guided_at: DateTime<Utc>,
    pub parent_id: Option<Uuid>,
    pub depth: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TxOutput for Record {}

/// rust の `GuidanceRecordAttachment`
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attachment {
    pub id: Uuid,
    pub record_id: Uuid,
    pub file_name: String,
    pub file_type: String,
    pub file_size: Option<i32>,
    pub storage_url: String,
    pub created_at: DateTime<Utc>,
}

impl TxOutput for Attachment {}

/// 一覧の木の 1 つ (rust の `GuidanceRecordTree`。記録の列に `employee_name`・`children`・`attachments` を足した形)
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tree {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub employee_id: Uuid,
    pub employee_name: Option<String>,
    pub guidance_type: String,
    pub title: String,
    pub content: String,
    pub guided_by: Option<String>,
    pub guided_at: DateTime<Utc>,
    pub parent_id: Option<Uuid>,
    pub depth: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub children: Vec<Tree>,
    pub attachments: Vec<Attachment>,
}

impl TxOutput for Tree {}

/// rust の `CreateGuidanceRecord`
#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub employee_id: Uuid,
    pub guidance_type: Option<String>,
    pub title: String,
    pub content: Option<String>,
    pub guided_by: Option<String>,
    pub guided_at: Option<DateTime<Utc>>,
    pub parent_id: Option<Uuid>,
}

/// rust の `UpdateGuidanceRecord`
#[derive(Debug, Deserialize)]
pub struct UpdateBody {
    pub guidance_type: Option<String>,
    pub title: Option<String>,
    pub content: Option<String>,
    pub guided_by: Option<String>,
    pub guided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct Filter {
    pub employee_id: Option<Uuid>,
    pub guidance_type: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// rust の `GuidanceRecordsResponse`
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub records: Vec<Tree>,
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
            "employee_id, ",
            $p,
            "guidance_type, ",
            $p,
            "title, ",
            $p,
            "content, ",
            $p,
            "guided_by, ",
            $p,
            "guided_at, ",
            $p,
            "parent_id, ",
            $p,
            "depth, ",
            $p,
            "created_at, ",
            $p,
            "updated_at"
        )
    };
}

macro_rules! top_filter {
    () => {
        "g.tenant_id = $1 AND g.parent_id IS NULL \
         AND ($2::UUID IS NULL OR g.employee_id = $2) \
         AND ($3::TEXT IS NULL OR g.guidance_type = $3) \
         AND ($4::TEXT IS NULL OR g.guided_at >= $4::TIMESTAMPTZ) \
         AND ($5::TEXT IS NULL OR g.guided_at < ($5::DATE + INTERVAL '1 day'))"
    };
}

macro_rules! att_columns {
    () => {
        "a.id, a.record_id, a.file_name, a.file_type, a.file_size, a.storage_url, a.created_at"
    };
}

pub mod sql {
    pub const COUNT_TOP: &str = concat!(
        "SELECT COUNT(*) FROM alc_api.guidance_records g WHERE ",
        top_filter!()
    );

    pub const LIST_TREE: &str = concat!(
        "WITH RECURSIVE top AS (SELECT g.id FROM alc_api.guidance_records g WHERE ",
        top_filter!(),
        " ORDER BY g.guided_at DESC LIMIT $6 OFFSET $7), \
         tree AS (SELECT g.* FROM alc_api.guidance_records g WHERE g.id IN (SELECT id FROM top) \
           UNION ALL \
           SELECT g.* FROM alc_api.guidance_records g JOIN tree t ON g.parent_id = t.id \
           WHERE g.depth < 3 AND g.tenant_id = $1) \
         SELECT ",
        columns!("t."),
        ", e.name FROM tree t \
         LEFT JOIN alc_api.employees e ON e.id = t.employee_id AND e.tenant_id = t.tenant_id \
         ORDER BY t.depth, t.guided_at DESC"
    );

    pub const ATTACHMENTS_OF: &str = concat!(
        "SELECT ",
        att_columns!(),
        " FROM alc_api.guidance_record_attachments a \
         JOIN alc_api.guidance_records g ON g.id = a.record_id \
         WHERE g.tenant_id = $1 AND a.record_id = ANY($2) ORDER BY a.created_at"
    );

    pub const GET: &str = concat!(
        "SELECT ",
        columns!(""),
        " FROM alc_api.guidance_records WHERE tenant_id = $1 AND id = $2"
    );

    pub const PARENT_DEPTH: &str =
        "SELECT depth FROM alc_api.guidance_records WHERE tenant_id = $1 AND id = $2";

    pub const INSERT: &str = concat!(
        "INSERT INTO alc_api.guidance_records \
         (tenant_id, employee_id, guidance_type, title, content, guided_by, guided_at, parent_id, depth) \
         VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7, now()), $8, $9) RETURNING ",
        columns!("")
    );

    pub const UPDATE: &str = concat!(
        "UPDATE alc_api.guidance_records SET guidance_type = COALESCE($3, guidance_type), \
         title = COALESCE($4, title), content = COALESCE($5, content), guided_by = COALESCE($6, guided_by), \
         guided_at = COALESCE($7, guided_at), updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 RETURNING ",
        columns!("")
    );

    pub const DELETE_RECURSIVE: &str = "WITH RECURSIVE descendants AS ( \
         SELECT id FROM alc_api.guidance_records WHERE tenant_id = $1 AND id = $2 \
         UNION ALL \
         SELECT g.id FROM alc_api.guidance_records g JOIN descendants d ON g.parent_id = d.id WHERE g.tenant_id = $1) \
         DELETE FROM alc_api.guidance_records WHERE tenant_id = $1 AND id IN (SELECT id FROM descendants)";

    /// 記録がこの会社に在るか と、新しい添付の uuid
    pub const RECORD_AND_UUID: &str =
        "SELECT gen_random_uuid() FROM alc_api.guidance_records WHERE tenant_id = $1 AND id = $2";

    pub const INSERT_ATTACHMENT: &str = "INSERT INTO alc_api.guidance_record_attachments \
         (record_id, file_name, file_type, file_size, storage_url) VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, record_id, file_name, file_type, file_size, storage_url, created_at";

    pub const GET_ATTACHMENT: &str = concat!(
        "SELECT ",
        att_columns!(),
        " FROM alc_api.guidance_record_attachments a \
         JOIN alc_api.guidance_records g ON g.id = a.record_id \
         WHERE g.tenant_id = $1 AND a.record_id = $2 AND a.id = $3"
    );

    pub const DELETE_ATTACHMENT: &str = "DELETE FROM alc_api.guidance_record_attachments a \
         USING alc_api.guidance_records g \
         WHERE g.id = a.record_id AND g.tenant_id = $1 AND a.record_id = $2 AND a.id = $3";
}

fn record(r: &Row) -> Record {
    Record {
        id: r.get(0),
        tenant_id: r.get(1),
        employee_id: r.get(2),
        guidance_type: r.get(3),
        title: r.get(4),
        content: r.get(5),
        guided_by: r.get(6),
        guided_at: r.get(7),
        parent_id: r.get(8),
        depth: r.get(9),
        created_at: r.get(10),
        updated_at: r.get(11),
    }
}

fn attachment(r: &Row) -> Attachment {
    Attachment {
        id: r.get(0),
        record_id: r.get(1),
        file_name: r.get(2),
        file_type: r.get(3),
        file_size: r.get(4),
        storage_url: r.get(5),
        created_at: r.get(6),
    }
}

/// 記録 (と社員の名前) を、`parent` の子から木にする (rust の `build_tree`)
pub fn build_tree(
    records: &[(Record, Option<String>)],
    attachments: &[Attachment],
    parent: Option<Uuid>,
) -> Vec<Tree> {
    records
        .iter()
        .filter(|(r, _)| r.parent_id == parent)
        .map(|(r, name)| Tree {
            id: r.id,
            tenant_id: r.tenant_id,
            employee_id: r.employee_id,
            employee_name: name.clone(),
            guidance_type: r.guidance_type.clone(),
            title: r.title.clone(),
            content: r.content.clone(),
            guided_by: r.guided_by.clone(),
            guided_at: r.guided_at,
            parent_id: r.parent_id,
            depth: r.depth,
            created_at: r.created_at,
            updated_at: r.updated_at,
            children: build_tree(records, attachments, Some(r.id)),
            attachments: attachments
                .iter()
                .filter(|a| a.record_id == r.id)
                .cloned()
                .collect(),
        })
        .collect()
}

/// R2 のキー (rust と同じ `{tenant}/guidance/{record}/{uuid}.{ext}`。拡張子は最後の `.` より後)
pub fn attachment_key(tenant_id: Uuid, record_id: Uuid, file_uuid: Uuid, filename: &str) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("bin");
    format!("{tenant_id}/guidance/{record_id}/{file_uuid}.{ext}")
}

/// 保存した `storage_url` から R2 のキーを取り出す。URL の中で `/{tenant_id}/guidance/{record_id}/` が最後に現れる
/// 位置から後ろ (先頭の `/` は含めない) で、残りが空でなく `/`・`..` を含まないときだけ返す (別の会社・別の記録・
/// 形の崩れた URL は None = 呼び手は 500)
pub fn extract_key(url: &str, tenant_id: Uuid, record_id: Uuid) -> Option<String> {
    let marker = format!("/{tenant_id}/guidance/{record_id}/");
    let start = url.rfind(&marker)?;
    let name = &url[start + marker.len()..];
    let valid = !name.is_empty() && !name.contains('/') && !name.contains("..");
    valid.then(|| url[start + 1..].to_owned())
}

pub async fn list(pg: &mut PgClient, tenant_id: Uuid, f: Filter) -> Result<ListResponse, Error> {
    let (page, per_page) = paging(f.page, f.per_page);
    let offset = (page - 1) * per_page;
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let base: [(&(dyn ToSql + Sync), Type); 5] = [
                (&tenant_id, Type::UUID),
                (&f.employee_id, Type::UUID),
                (&f.guidance_type, Type::TEXT),
                (&f.date_from, Type::TEXT),
                (&f.date_to, Type::TEXT),
            ];
            let total: i64 = tx.query_typed_one(sql::COUNT_TOP, &base).await?.get(0);
            let mut all = base.to_vec();
            all.push((&per_page, Type::INT8));
            all.push((&offset, Type::INT8));
            let rows = tx.query_typed(sql::LIST_TREE, &all).await?;
            let records: Vec<(Record, Option<String>)> =
                rows.iter().map(|r| (record(r), r.get(12))).collect();
            let ids: Vec<Uuid> = records.iter().map(|(r, _)| r.id).collect();
            let atts = tx
                .query_typed(
                    sql::ATTACHMENTS_OF,
                    &[(&tenant_id, Type::UUID), (&ids, Type::UUID_ARRAY)],
                )
                .await?;
            let atts: Vec<Attachment> = atts.iter().map(attachment).collect();
            Ok((total, build_tree(&records, &atts, None)))
        })
    })
    .await
    .map(|(total, records)| ListResponse {
        records,
        total,
        page,
        per_page,
    })
}

/// 1 行の記録を返す文 (`$1` = tenant・`$2` = id に続けて `extra`)
async fn one_record(
    pg: &mut PgClient,
    tenant_id: Uuid,
    stmt: &'static str,
    id: Uuid,
    extra: Vec<(Box<dyn ToSql + Send + Sync>, Type)>,
) -> Result<Option<Record>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let mut params: Vec<(&(dyn ToSql + Sync), Type)> =
                vec![(&tenant_id, Type::UUID), (&id, Type::UUID)];
            params.extend(
                extra
                    .iter()
                    .map(|(v, t)| (v.as_ref() as &(dyn ToSql + Sync), t.clone())),
            );
            let row = tx.query_typed_opt(stmt, &params).await?;
            Ok(row.as_ref().map(record))
        })
    })
    .await
}

fn arg<T: ToSql + Send + Sync + 'static>(v: T, t: Type) -> (Box<dyn ToSql + Send + Sync>, Type) {
    (Box::new(v), t)
}

/// 作成の結果
#[derive(Debug, Clone, PartialEq)]
pub enum Created {
    Done(Box<Record>),
    /// 親が無い (404)
    NoParent,
    /// 親が 3 階層目 (400)
    TooDeep,
}

impl TxOutput for Created {}

async fn parent_depth(
    tx: &TenantTx<'_>,
    tenant_id: Uuid,
    parent: Uuid,
) -> Result<Option<i32>, Error> {
    let row = tx
        .query_typed_opt(
            sql::PARENT_DEPTH,
            &[(&tenant_id, Type::UUID), (&parent, Type::UUID)],
        )
        .await?;
    Ok(row.map(|r| r.get(0)))
}

/// 作成 (親の深さの確かめと作成を 1 つの transaction で)
pub async fn create(pg: &mut PgClient, tenant_id: Uuid, b: CreateBody) -> Result<Created, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let depth = match b.parent_id {
                None => 0,
                Some(parent) => match parent_depth(tx, tenant_id, parent).await? {
                    None => return Ok(Created::NoParent),
                    Some(d) if d >= 2 => return Ok(Created::TooDeep),
                    Some(d) => d + 1,
                },
            };
            let guidance_type = b.guidance_type.unwrap_or_else(|| "general".to_owned());
            let content = b.content.unwrap_or_default();
            let row = tx
                .query_typed_one(
                    sql::INSERT,
                    &[
                        (&tenant_id, Type::UUID),
                        (&b.employee_id, Type::UUID),
                        (&guidance_type, Type::TEXT),
                        (&b.title, Type::TEXT),
                        (&content, Type::TEXT),
                        (&b.guided_by, Type::TEXT),
                        (&b.guided_at, Type::TIMESTAMPTZ),
                        (&b.parent_id, Type::UUID),
                        (&depth, Type::INT4),
                    ],
                )
                .await?;
            Ok(Created::Done(Box::new(record(&row))))
        })
    })
    .await
}

/// `$1` = tenant・`$2` = id (・`$3` = 添付) で、変えた行の数を返す文
async fn affected(
    pg: &mut PgClient,
    tenant_id: Uuid,
    stmt: &'static str,
    id: Uuid,
    att: Option<Uuid>,
) -> Result<u64, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let mut params: Vec<(&(dyn ToSql + Sync), Type)> =
                vec![(&tenant_id, Type::UUID), (&id, Type::UUID)];
            if let Some(a) = &att {
                params.push((a, Type::UUID));
            }
            tx.execute_typed(stmt, &params).await
        })
    })
    .await
}

pub async fn attachments(
    pg: &mut PgClient,
    tenant_id: Uuid,
    record_id: Uuid,
) -> Result<Vec<Attachment>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let ids = vec![record_id];
            let rows = tx
                .query_typed(
                    sql::ATTACHMENTS_OF,
                    &[(&tenant_id, Type::UUID), (&ids, Type::UUID_ARRAY)],
                )
                .await?;
            Ok(rows.iter().map(attachment).collect())
        })
    })
    .await
}

pub async fn get_attachment(
    pg: &mut PgClient,
    tenant_id: Uuid,
    record_id: Uuid,
    att_id: Uuid,
) -> Result<Option<Attachment>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let row = tx
                .query_typed_opt(
                    sql::GET_ATTACHMENT,
                    &[
                        (&tenant_id, Type::UUID),
                        (&record_id, Type::UUID),
                        (&att_id, Type::UUID),
                    ],
                )
                .await?;
            Ok(row.as_ref().map(attachment))
        })
    })
    .await
}

/// 記録がこの会社に在れば、新しい添付の uuid
pub async fn record_and_uuid(
    pg: &mut PgClient,
    tenant_id: Uuid,
    record_id: Uuid,
) -> Result<Option<Uuid>, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let row = tx
                .query_typed_opt(
                    sql::RECORD_AND_UUID,
                    &[(&tenant_id, Type::UUID), (&record_id, Type::UUID)],
                )
                .await?;
            Ok(row.map(|r| r.get(0)))
        })
    })
    .await
}

pub struct NewAttachment {
    pub record_id: Uuid,
    pub file_name: String,
    pub file_type: String,
    pub file_size: i32,
    pub storage_url: String,
}

pub async fn insert_attachment(
    pg: &mut PgClient,
    tenant_id: Uuid,
    n: NewAttachment,
) -> Result<Attachment, Error> {
    pg.tenant_tx(tenant_id, move |tx| {
        Box::pin(async move {
            let row = tx
                .query_typed_one(
                    sql::INSERT_ATTACHMENT,
                    &[
                        (&n.record_id, Type::UUID),
                        (&n.file_name, Type::TEXT),
                        (&n.file_type, Type::TEXT),
                        (&n.file_size, Type::INT4),
                        (&n.storage_url, Type::TEXT),
                    ],
                )
                .await?;
            Ok(attachment(&row))
        })
    })
    .await
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/guidance-records", get(list_records).post(create_record))
        .route(
            "/guidance-records/{id}",
            get(get_record).put(update_record).delete(delete_record),
        )
        .route(
            "/guidance-records/{id}/attachments",
            get(list_attachments).post(upload_attachment),
        )
        .route(
            "/guidance-records/{id}/attachments/{att_id}",
            get(download_attachment).delete(delete_attachment),
        )
}

async fn list_records(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Query(f): Query<Filter>,
) -> Result<Json<ListResponse>, StatusCode> {
    let mut pg = state.pg.lock().await;
    list(&mut pg, tenant_id, f)
        .await
        .map(Json)
        .map_err(|e| state.db_fail("guidance list", &e))
}

async fn get_record(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Record>, StatusCode> {
    let mut pg = state.pg.lock().await;
    one_record(&mut pg, tenant_id, sql::GET, id, Vec::new())
        .await
        .map_err(|e| state.db_fail("guidance get", &e))?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn create_record(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<Record>), StatusCode> {
    let mut pg = state.pg.lock().await;
    match create(&mut pg, tenant_id, b)
        .await
        .map_err(|e| state.db_fail("guidance create", &e))?
    {
        Created::Done(r) => Ok((StatusCode::CREATED, Json(*r))),
        Created::NoParent => Err(StatusCode::NOT_FOUND),
        Created::TooDeep => Err(StatusCode::BAD_REQUEST),
    }
}

async fn update_record(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(b): Json<UpdateBody>,
) -> Result<Json<Record>, StatusCode> {
    let mut pg = state.pg.lock().await;
    one_record(
        &mut pg,
        tenant_id,
        sql::UPDATE,
        id,
        vec![
            arg(b.guidance_type, Type::TEXT),
            arg(b.title, Type::TEXT),
            arg(b.content, Type::TEXT),
            arg(b.guided_by, Type::TEXT),
            arg(b.guided_at, Type::TIMESTAMPTZ),
        ],
    )
    .await
    .map_err(|e| state.db_fail("guidance update", &e))?
    .map(Json)
    .ok_or(StatusCode::NOT_FOUND)
}

async fn deleted_or_404(
    state: AppState,
    tenant_id: Uuid,
    stmt: &'static str,
    id: Uuid,
    att: Option<Uuid>,
    stage: &str,
) -> Result<StatusCode, StatusCode> {
    let mut pg = state.pg.lock().await;
    let n = affected(&mut pg, tenant_id, stmt, id, att)
        .await
        .map_err(|e| state.db_fail(stage, &e))?;
    if n > 0 {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn delete_record(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    deleted_or_404(
        state,
        tenant_id,
        sql::DELETE_RECURSIVE,
        id,
        None,
        "guidance delete",
    )
    .await
}

async fn list_attachments(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Attachment>>, StatusCode> {
    let mut pg = state.pg.lock().await;
    attachments(&mut pg, tenant_id, id)
        .await
        .map(Json)
        .map_err(|e| state.db_fail("guidance attachments", &e))
}

/// 記録の確かめ (404) → R2 の binding (503) → multipart の読み (400) → R2 に置く → 行の作成
async fn upload_attachment(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path(record_id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<Attachment>), StatusCode> {
    let file_uuid = {
        let mut pg = state.pg.lock().await;
        record_and_uuid(&mut pg, tenant_id, record_id).await
    }
    .map_err(|e| state.db_fail("guidance attachment record", &e))?
    .ok_or(StatusCode::NOT_FOUND)?;
    let store = state.store("guidance attachment upload")?;
    let field = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .ok_or(StatusCode::BAD_REQUEST)?;
    let file_name = field.file_name().unwrap_or("file").to_owned();
    let file_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_owned();
    let data = field.bytes().await.map_err(|_| StatusCode::BAD_REQUEST)?;
    let file_size = data.len() as i32;
    let key = attachment_key(tenant_id, record_id, file_uuid, &file_name);
    store
        .put(&key, data.to_vec(), &file_type)
        .await
        .map_err(|e| state.store_fail("guidance attachment upload", e))?;
    let new = NewAttachment {
        record_id,
        file_name,
        file_type,
        file_size,
        storage_url: format!("{URL_PREFIX}{key}"),
    };
    let mut pg = state.pg.lock().await;
    let att = insert_attachment(&mut pg, tenant_id, new)
        .await
        .map_err(|e| state.db_fail("guidance attachment create", &e))?;
    Ok((StatusCode::CREATED, Json(att)))
}

/// 行 (404) → キーの取り出し (500) → R2 の binding (503) → R2 から読む
async fn download_attachment(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path((record_id, att_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, StatusCode> {
    let att = {
        let mut pg = state.pg.lock().await;
        get_attachment(&mut pg, tenant_id, record_id, att_id).await
    }
    .map_err(|e| state.db_fail("guidance attachment download", &e))?
    .ok_or(StatusCode::NOT_FOUND)?;
    let Some(key) = extract_key(&att.storage_url, tenant_id, record_id) else {
        (state.log)("records: guidance attachment download failed: key");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let store = state.store("guidance attachment download")?;
    let data = store
        .get(&key)
        .await
        .map_err(|e| state.store_fail("guidance attachment download", e))?;
    Ok((
        [
            (header::CONTENT_TYPE, att.file_type),
            (
                header::CONTENT_DISPOSITION,
                content_disposition(&att.file_name),
            ),
        ],
        data,
    ))
}

async fn delete_attachment(
    State(state): State<AppState>,
    Extension(TenantId(tenant_id)): Extension<TenantId>,
    Path((record_id, att_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    deleted_or_404(
        state,
        tenant_id,
        sql::DELETE_ATTACHMENT,
        record_id,
        Some(att_id),
        "guidance attachment delete",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Uuid = Uuid::from_u128(1);
    const R: Uuid = Uuid::from_u128(2);
    const F: Uuid = Uuid::from_u128(3);

    #[test]
    fn keys_are_the_rust_layout() {
        assert_eq!(
            attachment_key(T, R, F, "指導.v2.pdf"),
            format!("{T}/guidance/{R}/{F}.pdf")
        );
        assert_eq!(
            attachment_key(T, R, F, "README"),
            format!("{T}/guidance/{R}/{F}.README")
        );
    }

    #[test]
    fn keys_come_out_of_rust_and_worker_urls() {
        let key = attachment_key(T, R, F, "a.jpg");
        // worker が保存した URL と、rust が保存した公開 URL の形 (接頭辞は見ない)
        assert_eq!(
            extract_key(&format!("{URL_PREFIX}{key}"), T, R),
            Some(key.clone())
        );
        assert_eq!(
            extract_key(&format!("https://acct.r2.dev/bucket/{key}"), T, R),
            Some(key.clone())
        );
        // 別の会社・別の記録・形の崩れたもの
        assert_eq!(extract_key(&format!("{URL_PREFIX}{key}"), R, R), None);
        assert_eq!(extract_key(&format!("{URL_PREFIX}{key}"), T, F), None);
        assert_eq!(extract_key(&format!("x/{T}/guidance/{R}/"), T, R), None);
        assert_eq!(extract_key(&format!("x/{T}/guidance/{R}/a/b"), T, R), None);
        assert_eq!(extract_key(&format!("x/{T}/guidance/{R}/..a"), T, R), None);
    }
}
