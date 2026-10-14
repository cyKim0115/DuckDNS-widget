mod config;
mod duckdns;
mod install;
mod tray;

use config::{
    config_view, delete_token, load_config, parse_domains, save_config as write_config,
    store_token, validate_token, ConfigView,
};
use duckdns::{check, internal_error, DnsSnapshot};
use install::{
    autostart_disable, autostart_enable, autostart_is_enabled, cleanup_stale_debug_autostart,
    ensure_installed_release, guard_debug_requires_vite,
};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_window_state::StateFlags;

/// DuckDNS가 권하는 최소 간격과 같습니다. 더 자주 물어도 IP는 그리 자주 바뀌지 않습니다.
const POLL_INTERVAL_MS: u64 = 300_000;
const SETTINGS_LABEL: &str = "settings";

/// 네트워크 왕복이 여러 번이라 UI 스레드를 막지 않게 블로킹 풀에서 돌립니다.
#[tauri::command]
async fn get_status() -> DnsSnapshot {
    tauri::async_runtime::spawn_blocking(check)
        .await
        .unwrap_or_else(|e| internal_error(e.to_string()))
}

#[tauri::command]
async fn update_now() -> DnsSnapshot {
    tauri::async_runtime::spawn_blocking(duckdns::update_now)
        .await
        .unwrap_or_else(|e| internal_error(e.to_string()))
}

#[tauri::command]
fn get_config() -> ConfigView {
    config_view()
}

/// 토큰 칸이 비어 있으면 저장된 토큰을 그대로 둡니다. 설정 창이 토큰 원문을
/// 받지 않으니, 비운 채 저장했다고 지워 버리면 도메인만 고쳐도 토큰이 사라집니다.
#[tauri::command]
fn save_config(domains: String, token: Option<String>) -> Result<ConfigView, String> {
    let domains = parse_domains(&domains)?;
    let token = token
        .filter(|t| !t.trim().is_empty())
        .map(|t| validate_token(&t))
        .transpose()?;

    let mut config = load_config();
    config.domains = domains;
    write_config(&config)?;
    if let Some(token) = token {
        store_token(&token)?;
    }
    Ok(config_view())
}

#[tauri::command]
fn set_auto_update(enabled: bool) -> Result<ConfigView, String> {
    let mut config = load_config();
    config.auto_update = enabled;
    write_config(&config)?;
    Ok(config_view())
}

#[tauri::command]
fn clear_token() -> Result<ConfigView, String> {
    delete_token()?;
    Ok(config_view())
}

#[tauri::command]
fn get_poll_interval_ms() -> u64 {
    POLL_INTERVAL_MS
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// 설정 창은 tauri.conf.json에 `visible: false`로 미리 선언해 두고 show/hide만
/// 토글한다. 클릭마다 새로 만들면 WebView2 초기화 지연이 그대로 노출된다.
/// 숨겨 뒀던 창을 다시 쓰는 것이라, 값을 새로 읽으라고 이벤트도 함께 보낸다.
#[tauri::command]
fn open_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(SETTINGS_LABEL)
        .ok_or_else(|| "settings window not found".to_string())?;
    window.show().map_err(|e| e.to_string())?;
    window.unminimize().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;
    window.emit("settings-open", ()).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn close_settings_window(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(SETTINGS_LABEL) {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The preference lives in the webview, so the menu and startup both push it
/// onto the main window through here.
#[tauri::command]
fn set_always_on_top(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    window.set_always_on_top(enabled).map_err(|e| e.to_string())
}

#[tauri::command]
fn is_dev_build() -> bool {
    cfg!(debug_assertions)
}

#[tauri::command]
fn enable_autostart() -> Result<(), String> {
    autostart_enable()
}

#[tauri::command]
fn disable_autostart() -> Result<(), String> {
    autostart_disable()
}

#[tauri::command]
fn is_autostart_enabled() -> Result<bool, String> {
    autostart_is_enabled()
}

#[tauri::command]
fn install_release_copy() -> Result<String, String> {
    ensure_installed_release().map(|p| p.display().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    guard_debug_requires_vite();

    tauri::Builder::default()
        // 다른 플러그인보다 먼저 등록해야 두 번째 실행이 초기화 전에 끝납니다.
        // 트레이 아이콘 말고 재실행으로도 가려진 위젯을 꺼낼 수 있게,
        // 새로 띄우지 않고 기존 창을 앞으로 가져옵니다.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // 위치만 복원합니다. 창 높이는 도메인 수에 따라 프런트가 정하므로,
        // 크기까지 저장하면 지난 실행의 줄 수가 이번 실행을 덮어씁니다.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::POSITION)
                .with_denylist(&[SETTINGS_LABEL])
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            get_status,
            update_now,
            get_config,
            save_config,
            set_auto_update,
            clear_token,
            get_poll_interval_ms,
            quit_app,
            open_settings_window,
            close_settings_window,
            is_dev_build,
            enable_autostart,
            disable_autostart,
            is_autostart_enabled,
            install_release_copy,
            set_always_on_top
        ])
        .on_window_event(|window, event| {
            if window.label() != SETTINGS_LABEL {
                return;
            }
            // 설정 창을 닫아도 앱은 살아 있어야 한다. 파괴하지 않고 숨긴다.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            tray::install(app)?;
            cleanup_stale_debug_autostart();
            // Release builds keep a stable copy under LOCALAPPDATA for shortcuts/autostart.
            let _ = ensure_installed_release();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
