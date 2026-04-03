use crate::config::{AppState, load_config_and_apply_window_settings};
use crate::ipc::IpcStateWrapper;
use crate::ui::{initialize_audio_asset_manager, setup_window_event_listeners};
use crate::ui::exit_handler::setup_exit_handlers;
use crate::log_important;
use tauri::{AppHandle, Manager};

/// 应用设置和初始化
pub async fn setup_application(app_handle: &AppHandle) -> Result<(), String> {
    let state = app_handle.state::<AppState>();

    // 加载配置并应用窗口设置
    if let Err(e) = load_config_and_apply_window_settings(&state, app_handle).await {
        log_important!(warn, "加载配置失败: {}", e);
    }

    // 初始化音频资源管理器
    if let Err(e) = initialize_audio_asset_manager(app_handle) {
        log_important!(warn, "初始化音频资源管理器失败: {}", e);
    }

    // 设置窗口事件监听器
    setup_window_event_listeners(app_handle);

    // 设置退出处理器
    if let Err(e) = setup_exit_handlers(app_handle) {
        log_important!(warn, "设置退出处理器失败: {}", e);
    }

    // 启动 IPC 服务器（允许 MCP 服务器通过 socket 发送请求）
    {
        let ipc_state = app_handle.state::<IpcStateWrapper>();
        let ipc_inner = ipc_state.0.clone();
        if let Err(e) = crate::ipc::start_ipc_server(app_handle, ipc_inner).await {
            log_important!(warn, "启动 IPC 服务器失败: {}", e);
        }
    }

    Ok(())
}
