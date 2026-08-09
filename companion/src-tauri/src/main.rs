#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod companion_actor;
mod native_serial;

use companion_actor::{ActorCommand, CompanionActor};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_notification::NotificationExt;

#[derive(Clone)]
struct CompanionHandle(tokio::sync::mpsc::Sender<ActorCommand>);

#[tauri::command]
async fn connect_hidshift(state: tauri::State<'_, CompanionHandle>) -> Result<String, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    state
        .0
        .send(ActorCommand::Connect(send))
        .await
        .map_err(|_| "companion stopped".to_string())?;
    receive.await.map_err(|_| "companion stopped".to_string())?
}

#[tauri::command]
async fn management_request(
    state: tauri::State<'_, CompanionHandle>,
    request: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let request =
        hidshift::ManagementRequest::decode(&request).map_err(|error| format!("{error:?}"))?;
    let request_id = request.request_id;
    let (send, receive) = tokio::sync::oneshot::channel();
    state
        .0
        .send(ActorCommand::Request(request.command, send))
        .await
        .map_err(|_| "companion stopped".to_string())?;
    receive
        .await
        .map_err(|_| "companion stopped".to_string())?
        .map(|mut response| {
            response.request_id = request_id;
            response.encode().to_vec()
        })
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let autostart = app.autolaunch();
    if enabled {
        autostart.enable()
    } else {
        autostart.disable()
    }
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn request_notification_permission(app: tauri::AppHandle) -> Result<String, String> {
    app.notification()
        .request_permission()
        .map(|permission| format!("{permission:?}"))
        .map_err(|error| error.to_string())
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .setup(|app| {
            let (sender, receiver) = tokio::sync::mpsc::channel(32);
            app.manage(CompanionHandle(sender.clone()));
            tauri::async_runtime::spawn(CompanionActor::new(app.handle().clone(), receiver).run());
            tauri::async_runtime::spawn(async move {
                let (reply, _result) = tokio::sync::oneshot::channel();
                let _ = sender.send(ActorCommand::Connect(reply)).await;
            });

            let show = MenuItem::with_id(app, "show", "HIDShiftを開く", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect_hidshift,
            management_request,
            set_autostart,
            request_notification_permission
        ])
        .build(tauri::generate_context!())
        .expect("failed to build HIDShift Companion");

    app.run(|handle, event| match event {
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" => {
            api.prevent_close();
            if let Some(window) = handle.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        RunEvent::ExitRequested { .. } => {
            if let Some(state) = handle.try_state::<CompanionHandle>() {
                let _ = state.0.try_send(ActorCommand::Shutdown);
            }
        }
        _ => {}
    });
}
