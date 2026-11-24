#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod catalog;
mod control;
mod desktop;
mod disk;
mod disk_cache;
mod fan_catalog;
mod gpu;
mod helper;
mod hid;
mod hid_catalog;
mod intel_catalog;
mod monitor;
#[cfg(test)]
#[path = "../../control-runtime/src/policy.rs"]
mod policy;
mod preferences;
mod sampler;
mod service;
mod smc;

use monitor::Snapshot;
use std::sync::{Arc, Mutex};
use tauri::Manager;

type SharedMonitor = Arc<Mutex<sampler::Manager>>;

#[tauri::command]
async fn snapshot(state: tauri::State<'_, SharedMonitor>) -> Result<Snapshot, String> {
    let monitor = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut monitor = monitor.lock().map_err(|_| "采样状态锁已失效".to_string())?;
        monitor.request_snapshot()
    })
    .await
    .map_err(|error| format!("采样线程失败：{error}"))?
}

#[tauri::command]
async fn preferences_read(app: tauri::AppHandle) -> Result<preferences::Document, String> {
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("无法定位配置目录：{e}"))?;
    tauri::async_runtime::spawn_blocking(move || preferences::Store::new(directory).read())
        .await
        .map_err(|e| format!("配置读取线程失败：{e}"))?
}

#[tauri::command]
async fn preferences_update(
    app: tauri::AppHandle,
    expected_revision: serde_json::Value,
    update: serde_json::Value,
) -> Result<preferences::Document, String> {
    let expected_revision = expected_revision
        .as_u64()
        .ok_or_else(|| "配置修订号必须是非负整数".to_string())?;
    let update = serde_json::from_value::<preferences::Update>(update)
        .map_err(|_| "配置更新格式无效，请检查字段和类型".to_string())?;
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("无法定位配置目录：{e}"))?;
    tauri::async_runtime::spawn_blocking(move || {
        preferences::Store::new(directory).update(expected_revision, update)
    })
    .await
    .map_err(|e| format!("配置保存线程失败：{e}"))?
}

async fn service_operation(operation: service::Operation) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || service::request(operation))
        .await
        .map_err(|error| format!("服务管理线程失败：{error}"))?
}
#[tauri::command]
async fn service_status() -> Result<serde_json::Value, String> {
    service_operation(service::Operation::Status).await
}
#[tauri::command]
async fn service_register() -> Result<serde_json::Value, String> {
    service_operation(service::Operation::Register).await
}
#[tauri::command]
async fn service_unregister() -> Result<serde_json::Value, String> {
    service_operation(service::Operation::Unregister).await
}

#[tauri::command]
async fn helper_inspect() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(helper::inspect)
        .await
        .map_err(|error| format!("辅助服务线程失败：{error}"))?
}

#[tauri::command]
async fn control_status() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(control::status)
        .await
        .map_err(|e| format!("控制状态线程失败：{e}"))?
}
#[tauri::command]
async fn control_stop(cancel_request_id: Option<String>) -> Result<serde_json::Value, String> {
    let intent = control::reserve_stop(cancel_request_id.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || control::stop(intent))
        .await
        .map_err(|e| format!("停止控制线程失败：{e}"))?
}
#[tauri::command]
async fn control_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, SharedMonitor>,
    preset_id: String,
    expected_revision: serde_json::Value,
    request_id: String,
) -> Result<serde_json::Value, String> {
    let ticket = control::reserve_start(&request_id)?;
    let expected_revision = expected_revision
        .as_u64()
        .filter(|r| *r <= 9_007_199_254_740_991)
        .ok_or("预设修订号无效")?;
    let directory = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("无法定位配置目录：{e}"))?;
    let monitor = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        control::start(
            ticket,
            preferences::Store::new(directory),
            preset_id,
            expected_revision,
            || {
                monitor
                    .try_lock()
                    .map_err(|_| "采样繁忙，请稍后应用".to_string())?
                    .request_snapshot()
            },
        )
    })
    .await
    .map_err(|e| format!("应用预设线程失败：{e}"))?
}

#[tauri::command]
fn editor_state(state: tauri::State<'_, desktop::EditorState>, dirty: bool) {
    // Retain the existing wire argument; readiness, not a mirrored dirty bit,
    // protects edits whose notification has not reached the host yet.
    let _ = dirty;
    state.mark_ready();
}

#[tauri::command]
fn exit_after_discard(app: tauri::AppHandle, state: tauri::State<'_, desktop::EditorState>) {
    state.approve_exit();
    app.exit(0);
}

fn main() {
    if std::env::args().any(|arg| arg == "--sampler-worker") {
        if let Err(error) = sampler::run_worker() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if std::env::args().any(|arg| arg == "--snapshot") {
        let snapshot = {
            let mut manager = sampler::Manager::default();
            manager.request_snapshot()
        }; // Request worker retirement before either CLI exit path, including errors.
        match snapshot.and_then(|value| {
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
        }) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("快照失败：{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|arg| arg == "--service-status") {
        match service::request(service::Operation::Status) {
            Ok(value) => println!("{value}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|arg| arg == "--helper-inspect") {
        match helper::inspect() {
            Ok(value) => println!("{value}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|arg| arg == "--control-status") {
        match control::status() {
            Ok(value) => println!("{value}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    tauri::Builder::default()
        .manage(desktop::EditorState::default())
        .manage(Arc::new(Mutex::new(sampler::Manager::default())))
        .invoke_handler(tauri::generate_handler![
            snapshot,
            preferences_read,
            preferences_update,
            editor_state,
            exit_after_discard,
            service_status,
            service_register,
            service_unregister,
            helper_inspect,
            control_status,
            control_start,
            control_stop
        ])
        .setup(|app| {
            // A failed protected Quit menu is fatal; editing must never start
            // with the native predefined Quit route still bypassing the guard.
            desktop::install_application_menu(app)?;
            if let Err(error) = desktop::install(app) {
                eprintln!("菜单栏入口不可用，保留普通窗口行为：{error}");
            }
            Ok(())
        })
        .on_window_event(desktop::on_window_event)
        .build(tauri::generate_context!())
        .expect("无法启动 Macs Fan")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { api, .. } if desktop::should_prevent_exit(app) => {
                api.prevent_exit();
                desktop::request_editor_exit(app);
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => desktop::show(app),
            _ => {}
        });
}
