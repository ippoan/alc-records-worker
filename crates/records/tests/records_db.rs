//! 携行品・伝達事項・指導記録 (と添付) の口を、実 DB (`alc_api_rt` で接続) と R2 のモックで通す。期待値は rust-alc-api の
//! tests/mock_tests/mock_{carrying_items,communication_items,guidance_records}_test.rs に合わせる。
//! 接続先は `RECORDS_TEST_ADMIN_DATABASE_URL` (無ければ失敗する。tests/support)。

mod support;

use std::sync::atomic::Ordering;

use axum::http::header;
use serde_json::{json, Value};
use support::*;
use uuid::Uuid;

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

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

#[tokio::test]
async fn carrying_items_with_vehicle_conditions() {
    let app = app().await;
    let t = tenant().await;
    let a = ok(
        &app,
        "POST",
        "/api/carrying-items",
        t,
        Some(json!({"item_name": "免許証", "sort_order": 2,
                    "vehicle_conditions": [{"category": "car_kind", "value": "大型"}, {"category": "car_kind", "value": "大型"}, {"category": "use", "value": "貨物"}]})),
        201,
    )
    .await;
    assert_eq!(
        keys(&a),
        [
            "created_at",
            "id",
            "is_required",
            "item_name",
            "sort_order",
            "tenant_id",
            "vehicle_conditions"
        ]
    );
    // 同じ条件は 1 つ。is_required の既定は true
    assert_eq!(a["vehicle_conditions"].as_array().unwrap().len(), 2);
    assert_eq!(a["is_required"], true);
    let b = ok(
        &app,
        "POST",
        "/api/carrying-items",
        t,
        Some(json!({"item_name": "点検表", "sort_order": 1, "is_required": false})),
        201,
    )
    .await;
    assert_eq!(b["vehicle_conditions"], json!([]));

    let list = ok(&app, "GET", "/api/carrying-items", t, None, 200).await;
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["点検表", "免許証"]);
    let conds: Vec<&str> = list[1]["vehicle_conditions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["value"].as_str().unwrap())
        .collect();
    assert_eq!(conds, ["大型", "貨物"]);

    let uri = format!("/api/carrying-items/{}", a["id"].as_str().unwrap());
    // 条件を渡さなければ今の条件のまま、渡せば全部置き換える
    let u = ok(
        &app,
        "PUT",
        &uri,
        t,
        Some(json!({"item_name": "運転免許証"})),
        200,
    )
    .await;
    assert_eq!(
        (
            u["item_name"].clone(),
            u["vehicle_conditions"].as_array().unwrap().len()
        ),
        (json!("運転免許証"), 2)
    );
    let u = ok(
        &app,
        "PUT",
        &uri,
        t,
        Some(json!({"vehicle_conditions": [{"category": "use", "value": "乗用"}]})),
        200,
    )
    .await;
    assert_eq!(u["vehicle_conditions"][0]["value"], "乗用");
    assert_eq!(u["vehicle_conditions"].as_array().unwrap().len(), 1);

    let other = tenant().await;
    assert_eq!(
        ok(&app, "GET", "/api/carrying-items", other, None, 200).await,
        json!([])
    );
    assert_eq!(
        status(
            &app,
            "PUT",
            &uri,
            other,
            Some(json!({"vehicle_conditions": []}))
        )
        .await,
        404
    );
    assert_eq!(status(&app, "DELETE", &uri, other, None).await, 404);
    assert_eq!(status(&app, "DELETE", &uri, t, None).await, 204);
    assert_eq!(status(&app, "DELETE", &uri, t, None).await, 404);
    assert_eq!(status(&app, "PUT", &uri, t, Some(json!({}))).await, 404);
}

#[tokio::test]
async fn communication_items_list_active_crud() {
    let app = app().await;
    let t = tenant().await;
    let emp = employee(t, "山田").await;
    let other_emp = employee(t, "佐藤").await;
    let all = ok(
        &app,
        "POST",
        "/api/communication-items",
        t,
        Some(json!({"title": "全員へ"})),
        201,
    )
    .await;
    assert_eq!(
        (
            all["priority"].clone(),
            all["content"].clone(),
            all["is_active"].clone()
        ),
        (json!("normal"), json!(""), json!(true))
    );
    ok(&app, "POST", "/api/communication-items", t, Some(json!({"title": "急ぎ", "priority": "urgent", "target_employee_id": emp, "created_by": "管理者"})), 201).await;
    ok(
        &app,
        "POST",
        "/api/communication-items",
        t,
        Some(json!({"title": "他の人", "target_employee_id": other_emp})),
        201,
    )
    .await;
    ok(
        &app,
        "POST",
        "/api/communication-items",
        t,
        Some(json!({"title": "期限切れ", "effective_until": "2020-01-01T00:00:00Z"})),
        201,
    )
    .await;
    ok(
        &app,
        "POST",
        "/api/communication-items",
        t,
        Some(
            json!({"title": "まだ先", "priority": "low", "effective_from": "2099-01-01T00:00:00Z"}),
        ),
        201,
    )
    .await;

    let list = ok(&app, "GET", "/api/communication-items", t, None, 200).await;
    assert_eq!(
        (
            list["total"].clone(),
            list["page"].clone(),
            list["per_page"].clone()
        ),
        (json!(5), json!(1), json!(20))
    );
    let first = &list["items"][0];
    assert_eq!(
        (
            first["title"].clone(),
            first["target_employee_name"].clone()
        ),
        (json!("急ぎ"), json!("山田"))
    );
    assert!(list["items"][1]["target_employee_name"].is_null());
    // 宛先で絞る (その人宛てと全員宛て)。頁
    let mine = ok(
        &app,
        "GET",
        &format!("/api/communication-items?target_employee_id={emp}"),
        t,
        None,
        200,
    )
    .await;
    assert_eq!(mine["total"], 4);
    let page = ok(
        &app,
        "GET",
        "/api/communication-items?page=2&per_page=2",
        t,
        None,
        200,
    )
    .await;
    assert_eq!(
        (
            page["page"].clone(),
            page["items"].as_array().unwrap().len()
        ),
        (json!(2), 2)
    );

    // 有効なもの (期間の中) だけ。urgent が先
    let active = ok(
        &app,
        "GET",
        &format!("/api/communication-items/active?target_employee_id={emp}"),
        t,
        None,
        200,
    )
    .await;
    let titles: Vec<&str> = active
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["急ぎ", "全員へ"]);

    let uri = format!("/api/communication-items/{}", all["id"].as_str().unwrap());
    assert_eq!(ok(&app, "GET", &uri, t, None, 200).await["title"], "全員へ");
    let u = ok(&app, "PUT", &uri, t, Some(json!({"is_active": false, "target_employee_id": emp, "effective_from": "2026-01-01T00:00:00Z", "effective_until": "2027-01-01T00:00:00Z"})), 200).await;
    assert_eq!(
        (u["is_active"].clone(), u["target_employee_id"].clone()),
        (json!(false), json!(emp))
    );
    // 渡さない宛先・期間は変えない
    let u = ok(
        &app,
        "PUT",
        &uri,
        t,
        Some(json!({"title": "全員へ (改)"})),
        200,
    )
    .await;
    assert_eq!(
        (u["target_employee_id"].clone(), u["effective_from"].clone()),
        (json!(emp), json!("2026-01-01T00:00:00Z"))
    );
    assert_eq!(
        ok(
            &app,
            "GET",
            "/api/communication-items?is_active=false",
            t,
            None,
            200
        )
        .await["total"],
        1
    );

    let other = tenant().await;
    assert_eq!(
        ok(&app, "GET", "/api/communication-items", other, None, 200).await["total"],
        0
    );
    assert_eq!(status(&app, "GET", &uri, other, None).await, 404);
    assert_eq!(status(&app, "PUT", &uri, other, Some(json!({}))).await, 404);
    assert_eq!(status(&app, "DELETE", &uri, t, None).await, 204);
    assert_eq!(status(&app, "DELETE", &uri, t, None).await, 404);
}

#[tokio::test]
async fn guidance_records_tree_depth_and_recursive_delete() {
    let app = app().await;
    let t = tenant().await;
    let emp = employee(t, "鈴木").await;
    let top = ok(
        &app,
        "POST",
        "/api/guidance-records",
        t,
        Some(json!({"employee_id": emp, "title": "指導 1", "guided_at": "2026-10-01T00:00:00Z"})),
        201,
    )
    .await;
    assert_eq!(
        (
            top["depth"].clone(),
            top["guidance_type"].clone(),
            top["content"].clone()
        ),
        (json!(0), json!("general"), json!(""))
    );
    let tid = top["id"].as_str().unwrap().to_owned();
    let c1 = ok(
        &app,
        "POST",
        "/api/guidance-records",
        t,
        Some(json!({"employee_id": emp, "title": "子", "parent_id": tid})),
        201,
    )
    .await;
    let c2 = ok(
        &app,
        "POST",
        "/api/guidance-records",
        t,
        Some(json!({"employee_id": emp, "title": "孫", "parent_id": c1["id"]})),
        201,
    )
    .await;
    assert_eq!(c2["depth"], 2);
    // 3 階層まで (孫の下は 400)。親が無ければ 404
    assert_eq!(
        status(
            &app,
            "POST",
            "/api/guidance-records",
            t,
            Some(json!({"employee_id": emp, "title": "x", "parent_id": c2["id"]}))
        )
        .await,
        400
    );
    assert_eq!(
        status(
            &app,
            "POST",
            "/api/guidance-records",
            t,
            Some(json!({"employee_id": emp, "title": "x", "parent_id": Uuid::new_v4()}))
        )
        .await,
        404
    );
    ok(&app, "POST", "/api/guidance-records", t, Some(json!({"employee_id": emp, "title": "指導 2", "guidance_type": "accident", "guided_at": "2026-09-01T00:00:00Z"})), 201).await;

    let list = ok(&app, "GET", "/api/guidance-records", t, None, 200).await;
    assert_eq!(list["total"], 2);
    let first = &list["records"][0];
    assert_eq!(
        (first["title"].clone(), first["employee_name"].clone()),
        (json!("指導 1"), json!("鈴木"))
    );
    assert_eq!(first["children"][0]["children"][0]["title"], "孫");
    assert_eq!(first["attachments"], json!([]));
    // 絞り込み
    for (q, n) in [
        ("guidance_type=accident", 1),
        (&format!("employee_id={emp}") as &str, 2),
        ("date_from=2026-09-15T00:00:00Z", 1),
        ("date_to=2026-09-15", 1),
        ("page=2&per_page=1", 2),
    ] {
        let got = ok(
            &app,
            "GET",
            &format!("/api/guidance-records?{q}"),
            t,
            None,
            200,
        )
        .await;
        assert_eq!(got["total"], n, "{q}");
    }

    let uri = format!("/api/guidance-records/{tid}");
    assert_eq!(ok(&app, "GET", &uri, t, None, 200).await["title"], "指導 1");
    let u = ok(&app, "PUT", &uri, t, Some(json!({"content": "改"})), 200).await;
    assert_eq!(
        (u["content"].clone(), u["title"].clone()),
        (json!("改"), json!("指導 1"))
    );

    let other = tenant().await;
    assert_eq!(
        ok(&app, "GET", "/api/guidance-records", other, None, 200).await["total"],
        0
    );
    assert_eq!(status(&app, "GET", &uri, other, None).await, 404);
    assert_eq!(status(&app, "PUT", &uri, other, Some(json!({}))).await, 404);
    assert_eq!(status(&app, "DELETE", &uri, other, None).await, 404);
    // 下の記録も一緒に消える
    assert_eq!(status(&app, "DELETE", &uri, t, None).await, 204);
    assert_eq!(
        status(
            &app,
            "GET",
            &format!("/api/guidance-records/{}", c2["id"].as_str().unwrap()),
            t,
            None
        )
        .await,
        404
    );
    assert_eq!(status(&app, "DELETE", &uri, t, None).await, 404);
}

const BOUNDARY: &str = "records-test-boundary";

fn multipart(filename: Option<&str>, content_type: Option<&str>, data: &[u8]) -> Vec<u8> {
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
async fn guidance_attachments() {
    let app = app().await;
    let t = tenant().await;
    let emp = employee(t, "高橋").await;
    let rec = ok(
        &app,
        "POST",
        "/api/guidance-records",
        t,
        Some(json!({"employee_id": emp, "title": "添付"})),
        201,
    )
    .await;
    let rid = rec["id"].as_str().unwrap().to_owned();
    let base = format!("/api/guidance-records/{rid}/attachments");

    let got = upload(
        &app,
        t,
        &base,
        multipart(Some("記録.pdf"), Some("application/pdf"), b"PDF"),
    )
    .await;
    assert_eq!(got.status, 201);
    let att = got.json();
    assert_eq!(
        keys(&att),
        [
            "created_at",
            "file_name",
            "file_size",
            "file_type",
            "id",
            "record_id",
            "storage_url"
        ]
    );
    let url = att["storage_url"].as_str().unwrap();
    let key = url.strip_prefix("r2://alc-media/").unwrap();
    assert!(key.starts_with(&format!("{t}/guidance/{rid}/")) && key.ends_with(".pdf"));
    assert_eq!(
        app.store.object(key).unwrap(),
        (b"PDF".to_vec(), "application/pdf".to_owned())
    );
    let d = upload(&app, t, &base, multipart(None, None, b"x"))
        .await
        .json();
    assert_eq!(
        (d["file_name"].clone(), d["file_type"].clone()),
        (json!("file"), json!("application/octet-stream"))
    );

    assert_eq!(
        ok(&app, "GET", &base, t, None, 200)
            .await
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        ok(&app, "GET", "/api/guidance-records", t, None, 200).await["records"][0]["attachments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let one = format!("{base}/{}", att["id"].as_str().unwrap());
    let got = call(&app, "GET", &one, Some(t), None).await;
    assert_eq!((got.status, got.body.as_slice()), (200, b"PDF".as_slice()));
    assert_eq!(got.headers[header::CONTENT_TYPE], "application/pdf");
    assert!(got.headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .starts_with("inline; "));

    // rust が保存した公開 URL の形も読める。形の崩れた URL は 500
    let old_key = format!("{t}/guidance/{rid}/old.jpg");
    app.store
        .objects
        .lock()
        .unwrap()
        .insert(old_key.clone(), (b"OLD".to_vec(), "image/jpeg".to_owned()));
    let su = admin().await;
    let old: Uuid = su
        .query_one(
            "INSERT INTO alc_api.guidance_record_attachments (record_id, file_name, file_type, storage_url) \
             VALUES ($1::text::uuid, 'old.jpg', 'image/jpeg', $2) RETURNING id",
            &[&rid, &format!("https://acct.r2.dev/bucket/{old_key}")],
        )
        .await
        .unwrap()
        .get(0);
    let bad: Uuid = su
        .query_one(
            "INSERT INTO alc_api.guidance_record_attachments (record_id, file_name, file_type, storage_url) \
             VALUES ($1::text::uuid, 'bad', 'text/plain', 'https://x/y') RETURNING id",
            &[&rid],
        )
        .await
        .unwrap()
        .get(0);
    let got = call(&app, "GET", &format!("{base}/{old}"), Some(t), None).await;
    assert_eq!((got.status, got.body.as_slice()), (200, b"OLD".as_slice()));
    assert_eq!(
        call(&app, "GET", &format!("{base}/{bad}"), Some(t), None)
            .await
            .status,
        500
    );
    assert_eq!(
        app.logs(),
        ["records: guidance attachment download failed: key"]
    );

    // 別の会社・無い記録 → 404 (R2 に書かない)
    let other = tenant().await;
    assert_eq!(
        upload(&app, other, &base, multipart(Some("a"), None, b"a"))
            .await
            .status,
        404
    );
    assert_eq!(ok(&app, "GET", &base, other, None, 200).await, json!([]));
    assert_eq!(call(&app, "GET", &one, Some(other), None).await.status, 404);
    assert_eq!(status(&app, "DELETE", &one, other, None).await, 404);
    // multipart が読めない・空 → 400
    assert_eq!(
        upload(&app, t, &base, b"garbage".to_vec()).await.status,
        400
    );
    assert_eq!(
        upload(&app, t, &base, format!("--{BOUNDARY}--\r\n").into_bytes())
            .await
            .status,
        400
    );

    // R2 の失敗 → 500
    app.store.fail_put.store(true, Ordering::SeqCst);
    assert_eq!(
        upload(&app, t, &base, multipart(Some("a"), None, b"a"))
            .await
            .status,
        500
    );
    app.store.fail_get.store(true, Ordering::SeqCst);
    assert_eq!(call(&app, "GET", &one, Some(t), None).await.status, 500);

    // R2 の binding が無い → 503 (記録・行の確かめの後)
    let no_store = build_with(rt_client().await, false);
    assert_eq!(
        upload(&no_store, t, &base, multipart(Some("a"), None, b"a"))
            .await
            .status,
        503
    );
    assert_eq!(
        call(&no_store, "GET", &one, Some(t), None).await.status,
        503
    );
    // 行を作れない → 500 (R2 には置いた後)
    let ro = build(client_as(READ_ONLY_ROLE).await);
    assert_eq!(
        upload(&ro, t, &base, multipart(Some("a"), None, b"a"))
            .await
            .status,
        500
    );
    assert_eq!(
        ro.logs(),
        ["records: guidance attachment create failed: db (42501)"]
    );

    assert_eq!(status(&app, "DELETE", &one, t, None).await, 204);
    assert_eq!(status(&app, "DELETE", &one, t, None).await, 404);
}
