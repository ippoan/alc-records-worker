//! 物品と物品のファイルの口を、実 DB (`alc_api_rt` で接続) と R2 のモックで通す。期待値は rust-alc-api の
//! tests/mock_tests/mock_items_test.rs に合わせる。接続先は `RECORDS_TEST_ADMIN_DATABASE_URL` (無ければ失敗する。tests/support)。

mod support;

use std::sync::atomic::Ordering;

use axum::http::header;
use serde_json::{json, Value};
use support::*;
use uuid::Uuid;

const ITEM_KEYS: &[&str] = &[
    "barcode",
    "category",
    "created_at",
    "description",
    "id",
    "image_url",
    "item_type",
    "name",
    "owner_type",
    "owner_user_id",
    "parent_id",
    "quantity",
    "tenant_id",
    "updated_at",
    "url",
];

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

async fn ok(app: &App, method: &str, uri: &str, t: Uuid, body: Option<Value>, want: u16) -> Value {
    let got = call(app, method, uri, Some(t), body).await;
    assert_eq!(
        got.status,
        want,
        "{method} {uri}: {}",
        String::from_utf8_lossy(&got.body)
    );
    if got.body.is_empty() {
        Value::Null
    } else {
        got.json()
    }
}

async fn status(app: &App, method: &str, uri: &str, t: Uuid, body: Option<Value>) -> u16 {
    call(app, method, uri, Some(t), body).await.status
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn every_route_needs_the_tenant_header() {
    let app = app().await;
    for uri in [
        "/api/items",
        "/api/carrying-items",
        "/api/communication-items",
        "/api/guidance-records",
    ] {
        assert_eq!(
            call(&app, "GET", uri, None, None).await.status,
            401,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn items_tree_crud_search_move_ownership_convert() {
    let app = app().await;
    let t = tenant().await;

    let folder = ok(
        &app,
        "POST",
        "/api/items",
        t,
        Some(json!({"name": "棚 A", "item_type": "folder"})),
        201,
    )
    .await;
    assert_eq!(keys(&folder), ITEM_KEYS);
    // 既定の値 (rust と同じ)
    assert_eq!(
        (
            folder["owner_type"].clone(),
            folder["barcode"].clone(),
            folder["quantity"].clone()
        ),
        (json!("org"), json!(""), json!(1))
    );
    let fid = folder["id"].as_str().unwrap().to_owned();
    let inner = ok(
        &app,
        "POST",
        "/api/items",
        t,
        Some(json!({"name": "ドライバー", "parent_id": fid, "barcode": "4901", "quantity": 3})),
        201,
    )
    .await;
    let iid = inner["id"].as_str().unwrap().to_owned();
    ok(
        &app,
        "POST",
        "/api/items",
        t,
        Some(json!({"name": "アルコール検知器"})),
        201,
    )
    .await;
    ok(
        &app,
        "POST",
        "/api/items",
        t,
        Some(json!({"name": "私物", "owner_type": "personal"})),
        201,
    )
    .await;

    // 一覧: 1 つのフォルダの中だけ、種類の逆順 (item → folder。rust と同じ)・名前の順。持ち主の種類で絞る
    assert_eq!(
        names(&ok(&app, "GET", "/api/items", t, None, 200).await),
        ["アルコール検知器", "棚 A"]
    );
    assert_eq!(
        names(
            &ok(
                &app,
                "GET",
                &format!("/api/items?parent_id={fid}"),
                t,
                None,
                200
            )
            .await
        ),
        ["ドライバー"]
    );
    assert_eq!(
        names(&ok(&app, "GET", "/api/items?owner_type=personal", t, None, 200).await),
        ["私物"]
    );
    assert_eq!(
        names(&ok(&app, "GET", "/api/items/search?barcode=4901", t, None, 200).await),
        ["ドライバー"]
    );

    let uri = format!("/api/items/{iid}");
    assert_eq!(ok(&app, "GET", &uri, t, None, 200).await["quantity"], 3);
    let u = ok(&app, "PUT", &uri, t, Some(json!({"description": "+"})), 200).await;
    assert_eq!(
        (u["description"].clone(), u["name"].clone()),
        (json!("+"), json!("ドライバー"))
    );

    // 移す・持ち主を変える
    let moved = ok(
        &app,
        "POST",
        &format!("{uri}/move"),
        t,
        Some(json!({"parent_id": null})),
        200,
    )
    .await;
    assert!(moved["parent_id"].is_null());
    ok(
        &app,
        "POST",
        &format!("{uri}/move"),
        t,
        Some(json!({"parent_id": fid})),
        200,
    )
    .await;
    let owned = ok(
        &app,
        "POST",
        &format!("{uri}/ownership"),
        t,
        Some(json!({"owner_type": "personal"})),
        200,
    )
    .await;
    assert_eq!(owned["owner_type"], "personal");

    // フォルダを物品に変えると、中のものを 1 つ上へ移す
    let conv = ok(
        &app,
        "POST",
        &format!("/api/items/{fid}/convert"),
        t,
        Some(json!({"item_type": "item"})),
        200,
    )
    .await;
    assert_eq!(
        (
            conv["children_moved"].clone(),
            conv["item"]["item_type"].clone()
        ),
        (json!(1), json!("item"))
    );
    assert!(ok(&app, "GET", &uri, t, None, 200).await["parent_id"].is_null());
    // 中の無いものを変える・物品をフォルダに変える
    let conv = ok(
        &app,
        "POST",
        &format!("/api/items/{fid}/convert"),
        t,
        Some(json!({"item_type": "item"})),
        200,
    )
    .await;
    assert_eq!(conv["children_moved"], 0);
    let conv = ok(
        &app,
        "POST",
        &format!("/api/items/{fid}/convert"),
        t,
        Some(json!({"item_type": "folder"})),
        200,
    )
    .await;
    assert_eq!(conv["children_moved"], 0);

    // 別の会社からは見えない・変えられない
    let other = tenant().await;
    for (method, path, body) in [
        ("GET", uri.clone(), None),
        ("PUT", uri.clone(), Some(json!({}))),
        (
            "POST",
            format!("{uri}/move"),
            Some(json!({"parent_id": null})),
        ),
        (
            "POST",
            format!("{uri}/ownership"),
            Some(json!({"owner_type": "org"})),
        ),
        (
            "POST",
            format!("{uri}/convert"),
            Some(json!({"item_type": "item"})),
        ),
        ("DELETE", uri.clone(), None),
    ] {
        assert_eq!(
            status(&app, method, &path, other, body).await,
            404,
            "{method} {path}"
        );
    }
    assert_eq!(
        ok(
            &app,
            "GET",
            "/api/items/search?barcode=4901",
            other,
            None,
            200
        )
        .await,
        json!([])
    );

    assert_eq!(status(&app, "DELETE", &uri, t, None).await, 204);
    assert_eq!(status(&app, "GET", &uri, t, None).await, 404);
    assert_eq!(status(&app, "DELETE", &uri, t, None).await, 404);
    assert!(app.logs().is_empty(), "{:?}", app.logs());
}

const BOUNDARY: &str = "records-test-boundary";

pub fn multipart(filename: Option<&str>, content_type: Option<&str>, data: &[u8]) -> Vec<u8> {
    let mut head = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"");
    if let Some(f) = filename {
        head.push_str(&format!("; filename=\"{f}\""));
    }
    head.push_str("\r\n");
    if let Some(ct) = content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    head.push_str("\r\n");
    let mut body = head.into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn upload(app: &App, t: Uuid, uri: &str, body: Vec<u8>) -> Got {
    let ct = format!("multipart/form-data; boundary={BOUNDARY}");
    call_raw(app, "POST", uri, Some(t), Some(&ct), body).await
}

#[tokio::test]
async fn item_files_upload_and_download() {
    let app = app().await;
    let t = tenant().await;
    let got = upload(
        &app,
        t,
        "/api/item-files",
        multipart(Some("写真 1.jpg"), Some("image/jpeg"), b"JPG"),
    )
    .await;
    assert_eq!(got.status, 201);
    let f = got.json();
    assert_eq!(
        (
            f["filename"].clone(),
            f["content_type"].clone(),
            f["size_bytes"].clone()
        ),
        (json!("写真 1.jpg"), json!("image/jpeg"), json!(3))
    );
    let id = f["id"].as_str().unwrap();
    assert_eq!(
        app.store.object(&format!("items/{t}/{id}")).unwrap(),
        (b"JPG".to_vec(), "image/jpeg".to_owned())
    );
    // 名前・Content-Type が無ければ既定の値
    let d = upload(&app, t, "/api/item-files", multipart(None, None, b"x"))
        .await
        .json();
    assert_eq!(
        (d["filename"].clone(), d["content_type"].clone()),
        (json!("file"), json!("application/octet-stream"))
    );

    let got = call(&app, "GET", &format!("/api/item-files/{id}"), Some(t), None).await;
    assert_eq!((got.status, got.body.as_slice()), (200, b"JPG".as_slice()));
    assert_eq!(got.headers[header::CONTENT_TYPE], "image/jpeg");
    let cd = got.headers[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(cd.starts_with("inline; filename*=UTF-8''") && cd.ends_with("; filename=\"__ 1.jpg\""));
    // 別の会社・無いもの → 404
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/item-files/{id}"),
            Some(tenant().await),
            None
        )
        .await
        .status,
        404
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/item-files/{}", Uuid::new_v4()),
            Some(t),
            None
        )
        .await
        .status,
        404
    );
    // multipart が読めない・空 → 400
    assert_eq!(
        upload(&app, t, "/api/item-files", b"garbage".to_vec())
            .await
            .status,
        400
    );
    assert_eq!(
        upload(
            &app,
            t,
            "/api/item-files",
            format!("--{BOUNDARY}--\r\n").into_bytes()
        )
        .await
        .status,
        400
    );

    // R2 の失敗 → 500 (置けないときは行が残る = rust と同じ)
    app.store.fail_put.store(true, Ordering::SeqCst);
    assert_eq!(
        upload(&app, t, "/api/item-files", multipart(Some("a"), None, b"a"))
            .await
            .status,
        500
    );
    app.store.fail_get.store(true, Ordering::SeqCst);
    assert_eq!(
        call(&app, "GET", &format!("/api/item-files/{id}"), Some(t), None)
            .await
            .status,
        500
    );
    assert_eq!(
        app.logs(),
        [
            "records: item-files upload failed: r2 (put)",
            "records: item-files download failed: r2 (get)"
        ]
    );

    // R2 の binding が無い → 503 (取り出しは行の確かめの後)
    let no_store = build_with(rt_client().await, false);
    assert_eq!(
        upload(
            &no_store,
            t,
            "/api/item-files",
            multipart(Some("a"), None, b"a")
        )
        .await
        .status,
        503
    );
    assert_eq!(
        call(
            &no_store,
            "GET",
            &format!("/api/item-files/{id}"),
            Some(t),
            None
        )
        .await
        .status,
        503
    );
    // 行を作れない → 500
    let ro = build(client_as(READ_ONLY_ROLE).await);
    assert_eq!(
        upload(&ro, t, "/api/item-files", multipart(Some("a"), None, b"a"))
            .await
            .status,
        500
    );
    assert_eq!(ro.logs(), ["records: item-files create failed: db (42501)"]);
}

#[tokio::test]
async fn db_failures_are_500_with_fixed_logs() {
    let app = build(broken_client().await);
    let t = tenant().await;
    let id = Uuid::new_v4();
    let calls: Vec<(&str, String, Option<Value>)> = vec![
        ("GET", "/api/items".into(), None),
        ("GET", "/api/items/search?barcode=x".into(), None),
        ("POST", "/api/items".into(), Some(json!({"name": "a"}))),
        ("GET", format!("/api/items/{id}"), None),
        ("PUT", format!("/api/items/{id}"), Some(json!({}))),
        ("DELETE", format!("/api/items/{id}"), None),
        (
            "POST",
            format!("/api/items/{id}/move"),
            Some(json!({"parent_id": null})),
        ),
        (
            "POST",
            format!("/api/items/{id}/ownership"),
            Some(json!({"owner_type": "org"})),
        ),
        (
            "POST",
            format!("/api/items/{id}/convert"),
            Some(json!({"item_type": "item"})),
        ),
        ("GET", format!("/api/item-files/{id}"), None),
        ("GET", "/api/carrying-items".into(), None),
        (
            "POST",
            "/api/carrying-items".into(),
            Some(json!({"item_name": "a"})),
        ),
        ("PUT", format!("/api/carrying-items/{id}"), Some(json!({}))),
        ("DELETE", format!("/api/carrying-items/{id}"), None),
        ("GET", "/api/communication-items".into(), None),
        ("GET", "/api/communication-items/active".into(), None),
        (
            "POST",
            "/api/communication-items".into(),
            Some(json!({"title": "a"})),
        ),
        ("GET", format!("/api/communication-items/{id}"), None),
        (
            "PUT",
            format!("/api/communication-items/{id}"),
            Some(json!({})),
        ),
        ("DELETE", format!("/api/communication-items/{id}"), None),
        ("GET", "/api/guidance-records".into(), None),
        (
            "POST",
            "/api/guidance-records".into(),
            Some(json!({"employee_id": id, "title": "a"})),
        ),
        ("GET", format!("/api/guidance-records/{id}"), None),
        (
            "PUT",
            format!("/api/guidance-records/{id}"),
            Some(json!({})),
        ),
        ("DELETE", format!("/api/guidance-records/{id}"), None),
        (
            "GET",
            format!("/api/guidance-records/{id}/attachments"),
            None,
        ),
        (
            "GET",
            format!("/api/guidance-records/{id}/attachments/{id}"),
            None,
        ),
        (
            "DELETE",
            format!("/api/guidance-records/{id}/attachments/{id}"),
            None,
        ),
    ];
    let n = calls.len();
    for (method, uri, body) in calls {
        assert_eq!(
            call(&app, method, &uri, Some(t), body).await.status,
            500,
            "{method} {uri}"
        );
    }
    for uri in [
        "/api/item-files".to_owned(),
        format!("/api/guidance-records/{id}/attachments"),
    ] {
        assert_eq!(
            upload(&app, t, &uri, multipart(Some("a"), None, b"a"))
                .await
                .status,
            500,
            "{uri}"
        );
    }
    assert_eq!(app.logs().len(), n + 2);
    assert_logs_clean(&app, &[t.to_string(), id.to_string()]);
    assert!(app.logs().iter().all(|l| l.contains("failed: db (")));
}
