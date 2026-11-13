//! Synthetic-only wire host. Not compiled into the production daemon.
use super::*;
use crate::queue::{
    reader_authority::{Authority, Check, Phase},
    reader_repair::batch,
    reader_translation_map as map,
};
use serde_json::{json, Value};

fn dispatch_with(
    q: &mut Queue,
    library: &Path,
    wire: Value,
    origin: &Authority,
    authorize: impl Fn(Check<'_>) -> Result<Sensitivity>,
) -> Result<Value> {
    let Command::Reader {
        request,
        principal,
        max_sensitivity: ceiling,
    } = serde_json::from_value(wire)?
    else {
        anyhow::bail!("invalid_reader_request")
    };
    anyhow::ensure!(
        principal.as_deref() == Some(origin.principal())
            || (principal.is_none() && *origin == Authority::LocalOwner),
        "permission_denied"
    );
    let verifier = |check: Check<'_>| {
        anyhow::ensure!(check.authority == origin, "permission_denied");
        authorize(check)
    };
    let library = library.canonicalize()?;
    let value = match request {
        ReaderRequest::AlignBatchPreview(BatchPreviewRequest::Selection {
            item_id,
            revision_id,
        }) => batch::preview(
            q,
            &library,
            origin.clone(),
            &item_id,
            &revision_id,
            ceiling,
            &verifier,
        )?,
        ReaderRequest::AlignBatchPreview(BatchPreviewRequest::Status { item_id, batch_id }) => {
            batch::preview_status(
                q,
                &library,
                origin.principal(),
                &item_id,
                &batch_id,
                ceiling,
                &verifier,
            )?
        }
        ReaderRequest::AlignBatchStart {
            item_id,
            batch_id,
            request_id,
            accept_cost,
            accept_model_processing,
        } => batch::start_with(
            q,
            &library,
            origin.principal(),
            &item_id,
            &batch_id,
            &request_id,
            accept_cost,
            accept_model_processing,
            ceiling,
            |_, _| Ok(7),
            &verifier,
        )?,
        ReaderRequest::AlignBatchStatus {
            item_id,
            batch_id,
            offset,
            limit,
        } => batch::status(
            q,
            &library,
            origin.principal(),
            &item_id,
            &batch_id,
            offset,
            limit,
            ceiling,
            &verifier,
        )?,
        ReaderRequest::AlignBatchStop { item_id, batch_id } => batch::stop(
            q,
            &library,
            origin.principal(),
            &item_id,
            &batch_id,
            ceiling,
            &verifier,
        )?,
        ReaderRequest::TranslateMapPreview(selection) => serde_json::to_value(map::preview_with(
            q,
            &library,
            origin,
            selection,
            ceiling,
            100,
            |_, _| Ok(7),
            &verifier,
        )?)?,
        ReaderRequest::TranslateMapRequest(request) => {
            json!({"job":map::request_with(q, &library, origin, request, ceiling, 101, |_, _| Ok(7), &verifier)?})
        }
        ReaderRequest::TranslateMapStatus { item_id, job_id } => {
            json!({"job":map::status_with(q, &library, origin, &item_id, &job_id, ceiling, &verifier)?})
        }
        ReaderRequest::TranslateMapStop { item_id, job_id } => {
            json!({"job":map::stop_with(q, &library, origin, &item_id, &job_id, ceiling, &verifier)?})
        }
        ReaderRequest::TranslateMapRead {
            item_id,
            revision_id,
            chapter,
            translation_version,
            expected_version,
            offset,
            limit,
        } => {
            let ceiling = origin.check(&library, &item_id, Phase::Read, ceiling, &verifier)?;
            serde_json::to_value(store::chapter_translation_map::read(
                &library,
                &item_id,
                &revision_id,
                &chapter,
                &translation_version,
                &expected_version,
                offset,
                limit,
                ceiling,
            )?)?
        }
        _ => anyhow::bail!("invalid_reader_request"),
    };
    let mut response = Response::empty_ok();
    response.reader = Some(value);
    Ok(serde_json::to_value(response)?)
}

fn wire(op: Value) -> Value {
    json!({"command":"reader", "max_sensitivity":"public", "request":op})
}

fn setup() -> (tempfile::TempDir, Queue, PathBuf, store::ReaderDocument) {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("Library");
    let doc = format_fixture::seed(&library).unwrap();
    let mut q = Queue::open(&tmp.path().join("queue.db")).unwrap();
    q.replace_model_settings(
        0,
        memex_models::Configuration {
            providers: vec![memex_models::Provider {
                id: "fixture".into(),
                label: "Synthetic".into(),
                api_base: "https://synthetic.invalid/v1/".into(),
            }],
            models: vec![memex_models::Model {
                id: "low".into(),
                label: "Synthetic".into(),
                provider_id: "fixture".into(),
                model_id: "synthetic".into(),
            }],
            selections: vec![],
        },
    )
    .unwrap();
    let mut fixture_connection = rusqlite::Connection::open(q.database_path()).unwrap();
    memex_models::low_dial::replace(
        &mut fixture_connection,
        0,
        memex_models::low_dial::Profile {
            revision: 1,
            model: "low".into(),
            reasoning_effort: None,
            max_input_bytes: 8192,
            max_completion_tokens: 1024,
        },
    )
    .unwrap();
    (tmp, q, library, doc)
}

#[test]
fn frozen_extensions_parse_strictly_and_production_rejects_before_preparation() {
    let requests = [
        json!({"op":"align_batch_preview","item_id":"i","revision_id":"r"}),
        json!({"op":"align_batch_preview","item_id":"i","batch_id":"b"}),
        json!({"op":"align_batch_start","item_id":"i","batch_id":"b","request_id":"r","accept_cost":true,"accept_model_processing":true}),
        json!({"op":"align_batch_status","item_id":"i","batch_id":"b","offset":0,"limit":1}),
        json!({"op":"align_batch_stop","item_id":"i","batch_id":"b"}),
        json!({"op":"translate_map_preview","item_id":"i","revision_id":"r","chapter":"c","translation_version":"t"}),
        json!({"op":"translate_map_request","item_id":"i","revision_id":"r","chapter":"c","translation_version":"t","quote_id":"q","request_id":"r","accept_cost":true,"accept_model_processing":true,"expected_map_version":"missing"}),
        json!({"op":"translate_map_status","item_id":"i","job_id":"j"}),
        json!({"op":"translate_map_stop","item_id":"i","job_id":"j"}),
        json!({"op":"translate_map_read","item_id":"i","revision_id":"r","chapter":"c","translation_version":"t","expected_version":"any","offset":0,"limit":1}),
    ];
    let tmp = tempfile::tempdir().unwrap();
    for value in requests {
        let request: ReaderRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            super::dispatch(
                &tmp.path().join("nonexistent"),
                request,
                Sensitivity::Restricted,
                None
            )
            .unwrap_err()
            .to_string(),
            "permission_denied"
        );
        for field in ["authority", "origin", "bypass", "unknown"] {
            let mut bad = value.clone();
            bad[field] = json!(true);
            assert!(
                serde_json::from_value::<ReaderRequest>(bad).is_err(),
                "{field}"
            );
        }
    }
    for bad in [
        json!({"op":"align_batch_preview","item_id":"i","revision_id":"r","batch_id":"b"}),
        json!({"op":"align_batch_start","item_id":"i","batch_id":"b","request_id":"r","accept_cost":true}),
        json!({"op":"translate_map_request","item_id":"i","revision_id":"r","chapter":"c","translation_version":"t","quote_id":"q","request_id":"r","accept_cost":true,"expected_map_version":"missing"}),
    ] {
        assert!(serde_json::from_value::<ReaderRequest>(bad).is_err());
    }
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn batch_wire_round_trip_is_async_bound_and_stoppable() {
    let (_tmp, mut q, library, doc) = setup();
    let invoke = |q: &mut Queue, v: Value| {
        dispatch_with(
            q,
            &library,
            wire(v),
            &Authority::LocalOwner,
            crate::queue::reader_authority::local_only,
        )
    };
    let preview = invoke(
        &mut q,
        json!({"op":"align_batch_preview","item_id":doc.item_id,"revision_id":doc.revision_id}),
    )
    .unwrap();
    assert_eq!(preview["reader"]["state"], "preparing");
    assert!(preview["reader"]["quote"].is_null());
    let id = preview["reader"]["batch_id"].clone();
    batch::prepare_one(
        &q,
        &library,
        |_, _| Ok(7),
        crate::queue::reader_authority::local_only,
    )
    .unwrap();
    let ready = invoke(
        &mut q,
        json!({"op":"align_batch_preview","item_id":doc.item_id,"batch_id":id}),
    )
    .unwrap();
    assert_eq!(ready["reader"]["state"], "ready");
    assert_eq!(
        ready["reader"]["quote"]["chapters"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let start = json!({"op":"align_batch_start","item_id":doc.item_id,"batch_id":id,"request_id":"wire-start","accept_cost":true,"accept_model_processing":true});
    let mut no_consent = start.clone();
    no_consent["accept_model_processing"] = json!(false);
    assert!(invoke(&mut q, no_consent).is_err());
    assert_eq!(
        invoke(&mut q, start.clone()).unwrap()["reader"]["batch"]["state"],
        "queued"
    );
    assert_eq!(
        invoke(&mut q, start).unwrap()["reader"]["batch"]["state"],
        "queued"
    );
    let status =
        json!({"op":"align_batch_status","item_id":doc.item_id,"batch_id":id,"offset":0,"limit":1});
    assert_eq!(
        invoke(&mut q, status.clone()).unwrap()["reader"]["chapters"][0]["state"],
        "pending"
    );
    let mut invalid = status.clone();
    invalid["limit"] = json!(101);
    assert!(invoke(&mut q, invalid).is_err());
    let mut forged = wire(status);
    forged["principal"] = json!("other");
    assert!(dispatch_with(
        &mut q,
        &library,
        forged,
        &Authority::LocalOwner,
        |_| panic!("unbound principal")
    )
    .is_err());
    let stop = invoke(
        &mut q,
        json!({"op":"align_batch_stop","item_id":doc.item_id,"batch_id":id}),
    )
    .unwrap();
    assert_eq!(stop["reader"]["batch"]["state"], "stopped");
    assert_eq!(stop["reader"]["batch"]["usage"]["requests_dispatched"], 0);
}

#[test]
fn map_wire_round_trip_preserves_projection_and_requires_paged_version() {
    let (_tmp, mut q, library, doc) = setup();
    let source = store::chapter_translation::source(
        &library,
        &doc.item_id,
        &doc.revision_id,
        "appendix",
        Sensitivity::Public,
    )
    .unwrap();
    let projection = store::chapter_translation::publish(
        &library,
        source.identity,
        store::chapter_translation::Provenance {
            route: store::chapter_translation::Route {
                provider_id: "fixture".into(),
                model_id: "synthetic".into(),
                reasoning_effort: None,
            },
            prompt_version: "synthetic".into(),
            prompt_sha256: "a".repeat(64),
            plan_sha256: "b".repeat(64),
        },
        "# 附录\n\n合成正文。\n".into(),
        Sensitivity::Public,
    )
    .unwrap();
    let invoke = |q: &mut Queue, v: Value| {
        dispatch_with(
            q,
            &library,
            wire(v),
            &Authority::LocalOwner,
            crate::queue::reader_authority::local_only,
        )
    };
    let mut preview = json!({"op":"translate_map_preview","item_id":doc.item_id,"revision_id":doc.revision_id,"chapter":"appendix","translation_version":projection.version});
    let quote = invoke(&mut q, preview.clone()).unwrap()["reader"]["quote"].clone();
    preview["op"] = json!("translate_map_request");
    preview["quote_id"] = quote["quote_id"].clone();
    preview["request_id"] = json!("wire-map");
    preview["accept_cost"] = json!(true);
    preview["accept_model_processing"] = json!(true);
    preview["expected_map_version"] = json!("missing");
    let admitted = invoke(&mut q, preview).unwrap();
    let id = admitted["reader"]["job"]["job_id"].clone();
    assert_eq!(
        admitted["reader"]["job"]["operation"],
        "chapter_translation_map"
    );
    map::step_with(&mut q,&library,|_,_|Ok(7),crate::queue::reader_authority::local_only,|_,_,_,_,dispatch|{
        dispatch()?;Ok((r#"{"groups":[{"o":[0],"t":[0],"relation":"matched"},{"o":[1],"t":[1],"relation":"matched"}]}"#.into(),Default::default()))
    }).unwrap();
    map::step_with(
        &mut q,
        &library,
        |_, _| Ok(7),
        crate::queue::reader_authority::local_only,
        |_, _, _, _, _| panic!("second model call"),
    )
    .unwrap();
    assert_eq!(
        invoke(
            &mut q,
            json!({"op":"translate_map_status","item_id":doc.item_id,"job_id":id})
        )
        .unwrap()["reader"]["job"]["state"],
        "succeeded"
    );
    assert_eq!(
        invoke(
            &mut q,
            json!({"op":"translate_map_stop","item_id":doc.item_id,"job_id":id})
        )
        .unwrap()["reader"]["job"]["state"],
        "succeeded"
    );
    let mut read = json!({"op":"translate_map_read","item_id":doc.item_id,"revision_id":doc.revision_id,"chapter":"appendix","translation_version":projection.version,"expected_version":"any","offset":0,"limit":1});
    let first = invoke(&mut q, read.clone()).unwrap()["reader"].clone();
    assert_eq!(first["status"], "aligned");
    assert_eq!(first["certified"], false);
    read["offset"] = json!(1);
    assert!(invoke(&mut q, read.clone()).is_err());
    read["expected_version"] = first["version"].clone();
    assert_eq!(invoke(&mut q, read.clone()).unwrap()["reader"]["offset"], 1);
    assert!(dispatch_with(
        &mut q,
        &library,
        wire(read),
        &Authority::LocalOwner,
        |_| anyhow::bail!("revoked")
    )
    .is_err());
    assert_eq!(
        store::chapter_translation::metadata(
            &library,
            &doc.item_id,
            &doc.revision_id,
            "appendix",
            Sensitivity::Public
        )
        .unwrap()
        .unwrap()
        .version,
        projection.version
    );
}
