//! Create an item and its immutable request identity in the ToDo CLI's atomic
//! store transaction. A later block/done note change cannot erase that identity.
use crate::agent::{fault, invalid, io_fault, Paths, Result};
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) fn ensure(paths: &Paths, request: &Value) -> Result<Value> {
    let store = cc_todo::Paths::at(&paths.todo, &paths.jobs);
    let _lock =
        cc_todo::Lock::acquire(&store.lock, Duration::from_secs(30)).map_err(|_| io_fault())?;
    // This endpoint never initializes a missing production store.
    std::fs::metadata(&store.store).map_err(|_| io_fault())?;
    let mut db = cc_todo::load(&store.store).map_err(|_| io_fault())?;
    let id = request["request_id"].as_str().ok_or_else(invalid)?;
    let project = request["project"].as_str().ok_or_else(invalid)?;
    let title = request["title"].as_str().ok_or_else(invalid)?;
    let note = request["note"].as_str().ok_or_else(invalid)?;
    let items = db["items"].as_array().ok_or_else(io_fault)?;
    let matches: Vec<_> = items
        .iter()
        .filter(|item| item["console_creation_request"]["request_id"] == id)
        .collect();
    if matches.len() > 1 {
        return Err(io_fault());
    }
    if let Some(item) = matches.first() {
        if item["console_creation_request"] != *request || item["project"] != project {
            return Err(conflict());
        }
        return Ok(json!({"request_id":id,"item":item}));
    }

    // Mutable old notes cannot prove creation identity. A committed old
    // receipt is handled by the caller; never reattach or recreate from a note.
    let marker = format!("受付識別子: {id}");
    let legacy: Vec<_> = items
        .iter()
        .filter(|item| {
            item.get("console_creation_request").is_none()
                && item["note"]
                    .as_str()
                    .is_some_and(|v| v.lines().any(|line| line == marker))
        })
        .collect();
    if !legacy.is_empty() {
        return Err(fault(
            StatusCode::CONFLICT,
            "legacy_receipt_unverified",
            "旧版の受付を自動確認できません。再登録せず、保存した受付番号を確認してください。",
        ));
    }
    if db["projects"].get(project).is_none() {
        return Err(fault(
            StatusCode::NOT_FOUND,
            "project_not_found",
            "共有ToDoのCLIでプロジェクトを先に登録してください。",
        ));
    }
    let item_id = cc_todo::add_item(
        &mut db,
        &cc_todo::Command::Add {
            project: project.into(),
            title: title.into(),
            note: note.into(),
            spec: None,
            model: None,
            effort: None,
            needs: vec![],
            progress: None,
        },
        &store,
    )
    .map_err(|_| io_fault())?;
    let item = cc_todo::find(&mut db, item_id).map_err(|_| io_fault())?;
    item["console_creation_request"] = request.clone();
    let result = json!({"request_id":id,"item":item});
    cc_todo::save(&store.store, &db).map_err(|_| io_fault())?;
    Ok(result)
}

fn conflict() -> crate::agent::Fault {
    fault(
        StatusCode::CONFLICT,
        "request_id_conflict",
        "受付番号が別の作業に使われています。",
    )
}
