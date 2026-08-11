#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod companion_actor;
mod native_bluetooth;
mod native_hid;
mod native_notification;
mod native_state;
mod native_tray;

use companion_actor::{ActorCommand, CompanionActor};
use hidshift::HostId;
use hidshift_manager_ui::DestinationRoute;
use native_state::CompanionSnapshot;
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
async fn companion_snapshot(
    state: tauri::State<'_, CompanionHandle>,
) -> Result<CompanionSnapshot, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    state
        .0
        .send(ActorCommand::Snapshot(send))
        .await
        .map_err(|_| "companion stopped".to_string())?;
    receive.await.map_err(|_| "companion stopped".to_string())
}

#[tauri::command]
async fn set_notifications_enabled(
    app: tauri::AppHandle,
    state: tauri::State<'_, CompanionHandle>,
    enabled: bool,
) -> Result<CompanionSnapshot, String> {
    if enabled {
        let permission = app
            .notification()
            .request_permission()
            .map_err(|error| error.to_string())?;
        if permission != tauri::plugin::PermissionState::Granted {
            return Err("notification permission was not granted".into());
        }
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    state
        .0
        .send(ActorCommand::SetNotifications {
            enabled,
            reply: send,
        })
        .await
        .map_err(|_| "companion stopped".to_string())?;
    receive.await.map_err(|_| "companion stopped".to_string())?
}

#[tauri::command]
fn set_autostart(
    app: tauri::AppHandle,
    state: tauri::State<'_, CompanionHandle>,
    enabled: bool,
) -> Result<(), String> {
    let autostart = app.autolaunch();
    if enabled {
        autostart.enable()
    } else {
        autostart.disable()
    }
    .map_err(|error| error.to_string())?;
    state
        .0
        .try_send(ActorCommand::SetAutostartState(enabled))
        .map_err(|_| "companion stopped".to_string())
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

            let searching = MenuItem::with_id(
                app,
                "connection-status",
                "HIDShiftを検索中",
                false,
                None::<&str>,
            )?;
            let show = MenuItem::with_id(app, "show", "HIDShiftを開く", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&searching, &show, &quit])?;
            let tray = TrayIconBuilder::with_id("main");
            let tray = if let Some(icon) = app.default_window_icon() {
                tray.icon(icon.clone())
            } else {
                tray
            };
            tray.menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    "target-this-computer" => {
                        if let Some(state) = app.try_state::<CompanionHandle>() {
                            let _ = state.0.try_send(ActorCommand::SelectThisComputer);
                        }
                    }
                    "target-wired" => {
                        if let Some(state) = app.try_state::<CompanionHandle>() {
                            let _ = state
                                .0
                                .try_send(ActorCommand::SelectDestination(DestinationRoute::Wired));
                        }
                    }
                    id if id.starts_with("target-ble-") => {
                        if let Ok(slot) = id["target-ble-".len()..].parse::<u8>()
                            && let Some(state) = app.try_state::<CompanionHandle>()
                        {
                            let _ = state.0.try_send(ActorCommand::SelectDestination(
                                DestinationRoute::Ble(HostId(slot)),
                            ));
                        }
                    }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect_hidshift,
            management_request,
            companion_snapshot,
            set_notifications_enabled,
            set_autostart,
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
