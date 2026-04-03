use crate::config::{save_config, AppState, TelegramConfig};
use crate::constants::telegram as telegram_constants;
use crate::telegram::{
    DispatcherConfig, SessionAction, SessionEvent,
    get_or_init_dispatcher,
};
use crate::log_important;
use tauri::{AppHandle, Emitter, Manager, State};
use teloxide::prelude::*;

/// 获取Telegram配置
#[tauri::command]
pub async fn get_telegram_config(state: State<'_, AppState>) -> Result<TelegramConfig, String> {
    let config = state
        .config
        .lock()
        .map_err(|e| format!("获取配置失败: {}", e))?;
    Ok(config.telegram_config.clone())
}

/// 设置Telegram配置
#[tauri::command]
pub async fn set_telegram_config(
    telegram_config: TelegramConfig,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    {
        let mut config = state
            .config
            .lock()
            .map_err(|e| format!("获取配置失败: {}", e))?;
        config.telegram_config = telegram_config;
    }

    // 保存配置到文件
    save_config(&state, &app)
        .await
        .map_err(|e| format!("保存配置失败: {}", e))?;

    Ok(())
}

/// 测试Telegram Bot连接
#[tauri::command]
pub async fn test_telegram_connection_cmd(
    bot_token: String,
    chat_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // 获取API URL配置
    let api_url = {
        let config = state
            .config
            .lock()
            .map_err(|e| format!("获取配置失败: {}", e))?;
        config.telegram_config.api_base_url.clone()
    };

    // 使用默认API URL时传递None，否则传递自定义URL
    let api_url_option = if api_url == telegram_constants::API_BASE_URL {
        None
    } else {
        Some(api_url.as_str())
    };

    crate::telegram::core::test_telegram_connection_with_api_url(&bot_token, &chat_id, api_url_option)
        .await
        .map_err(|e| e.to_string())
}

/// 自动获取Chat ID（通过监听Bot消息）
#[tauri::command]
pub async fn auto_get_chat_id(
    bot_token: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    // 获取API URL配置
    let mut bot = Bot::new(bot_token.clone());

    if let Some(state) = app_handle.try_state::<AppState>() {
        if let Ok(config) = state.config.lock() {
            let api_url = &config.telegram_config.api_base_url;
            if api_url != telegram_constants::API_BASE_URL {
                if let Ok(url) = reqwest::Url::parse(api_url) {
                    bot = bot.set_api_url(url);
                }
            }
        }
    }

    // 发送事件通知前端开始监听
    if let Err(e) = app_handle.emit("chat-id-detection-started", ()) {
        log_important!(warn, "发送Chat ID检测开始事件失败: {}", e);
    }

    // 启动临时监听器来获取Chat ID
    let app_handle_clone = app_handle.clone();
    tokio::spawn(async move {
        let mut timeout_count = 0;
        const MAX_TIMEOUT_COUNT: u32 = 30; // 30秒超时

        loop {
            match bot.get_updates().send().await {
                Ok(updates) => {
                    for update in updates {
                        if let teloxide::types::UpdateKind::Message(message) = update.kind {
                            let chat_id = message.chat.id.0.to_string();
                            let chat_title = message.chat.title().unwrap_or("私聊").to_string();
                            let username = message.from.as_ref()
                                .and_then(|u| u.username.as_ref())
                                .map(|s| s.as_str())
                                .unwrap_or("未知用户");

                            // 发送检测到的Chat ID到前端
                            let chat_info = serde_json::json!({
                                "chat_id": chat_id,
                                "chat_title": chat_title,
                                "username": username,
                                "message_text": message.text().unwrap_or(""),
                            });

                            if let Err(e) = app_handle_clone.emit("chat-id-detected", chat_info) {
                                log_important!(warn, "发送Chat ID检测事件失败: {}", e);
                            }

                            return; // 检测到第一个消息后退出
                        }
                    }
                }
                Err(e) => {
                    log_important!(warn, "获取Telegram更新失败: {}", e);
                }
            }

            // 超时检查
            timeout_count += 1;
            if timeout_count >= MAX_TIMEOUT_COUNT {
                if let Err(e) = app_handle_clone.emit("chat-id-detection-timeout", ()) {
                    log_important!(warn, "发送Chat ID检测超时事件失败: {}", e);
                }
                break;
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    });

    Ok(())
}

/// 发送带 request_id 的 Telegram 事件到前端
/// 将 TelegramEvent 序列化后注入 request_id 字段，实现多弹窗事件路由
fn emit_telegram_event_with_id(
    app_handle: &AppHandle,
    event: &crate::telegram::TelegramEvent,
    request_id: &str,
) {
    if let Ok(mut value) = serde_json::to_value(event) {
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "request_id".to_string(),
                serde_json::Value::String(request_id.to_string()),
            );
        }
        let _ = app_handle.emit("telegram-event", &value);
    }
}

/// 发送Telegram消息（供其他模块调用）
pub async fn send_telegram_message(
    bot_token: &str,
    chat_id: &str,
    message: &str,
) -> Result<(), String> {
    send_telegram_message_with_markdown(bot_token, chat_id, message, false).await
}

/// 发送支持Markdown的Telegram消息
pub async fn send_telegram_message_with_markdown(
    bot_token: &str,
    chat_id: &str,
    message: &str,
    use_markdown: bool,
) -> Result<(), String> {
    use crate::telegram::TelegramCore;
    let core =
        TelegramCore::new(bot_token.to_string(), chat_id.to_string()).map_err(|e| e.to_string())?;

    core.send_message_with_markdown(message, use_markdown)
        .await
        .map_err(|e| e.to_string())
}

/// 启动Telegram同步（使用中心化调度器）
#[tauri::command]
pub async fn start_telegram_sync(
    message: String,
    predefined_options: Vec<String>,
    is_markdown: bool,
    client_name: Option<String>,
    request_id: Option<String>,
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<(), String> {
    // 获取Telegram配置
    let (enabled, bot_token, chat_id, api_url, continue_reply_enabled) = {
        let config = state
            .config
            .lock()
            .map_err(|e| format!("获取配置失败: {}", e))?;
        let api_url = if config.telegram_config.api_base_url == telegram_constants::API_BASE_URL {
            None
        } else {
            Some(config.telegram_config.api_base_url.clone())
        };
        (
            config.telegram_config.enabled,
            config.telegram_config.bot_token.clone(),
            config.telegram_config.chat_id.clone(),
            api_url,
            config.reply_config.enable_continue_reply,
        )
    };

    if !enabled {
        return Ok(());
    }

    if bot_token.trim().is_empty() || chat_id.trim().is_empty() {
        return Err("Telegram配置不完整".to_string());
    }

    // 获取或初始化全局调度器
    let dispatcher = get_or_init_dispatcher(DispatcherConfig {
        bot_token,
        chat_id,
        api_url,
    })
    .await
    .map_err(|e| format!("初始化Telegram调度器失败: {}", e))?;

    // 生成请求 ID（如果前端未传递）
    let req_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // 注册会话
    let (mut event_rx, req_short, seq_num) = dispatcher
        .register_session(req_id, predefined_options.clone(), continue_reply_enabled)
        .await;

    // 发送选项消息到 Telegram
    let send_result = dispatcher
        .send_options_message(
            &req_short,
            seq_num,
            &message,
            &predefined_options,
            is_markdown,
            client_name.as_deref(),
            continue_reply_enabled,
        )
        .await;

    if let Err(e) = send_result {
        // 发送失败，回滚会话注册
        dispatcher.unregister_session(&req_short).await;
        return Err(format!("发送选项消息失败: {}", e));
    }

    // 启动事件消费 task，将 dispatcher 事件转发到前端
    let req_short_clone = req_short.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                SessionEvent::Action(SessionAction::Send) => {
                    // 获取会话状态
                    if let Some(disp) = crate::telegram::get_dispatcher_async().await {
                        let state = disp.get_session_state(&req_short_clone).await;
                        let (selected_list, user_input) = state.unwrap_or_default();

                        // 发送反馈消息
                        let feedback = crate::telegram::core::build_feedback_message(
                            &selected_list, &user_input, false,
                        );
                        let _ = disp.send_message(&feedback).await;

                        // emit 前端事件（带 request_id 路由）
                        use crate::telegram::TelegramEvent;
                        emit_telegram_event_with_id(&app_handle, &TelegramEvent::SendPressed, &req_short_clone);

                        // 注销会话
                        disp.unregister_session(&req_short_clone).await;
                    }
                    break;
                }
                SessionEvent::Action(SessionAction::Continue) => {
                    if let Some(disp) = crate::telegram::get_dispatcher_async().await {
                        let feedback = crate::telegram::core::build_feedback_message(
                            &[], "", true,
                        );
                        let _ = disp.send_message(&feedback).await;

                        use crate::telegram::TelegramEvent;
                        emit_telegram_event_with_id(&app_handle, &TelegramEvent::ContinuePressed, &req_short_clone);

                        disp.unregister_session(&req_short_clone).await;
                    }
                    break;
                }
                SessionEvent::Telegram(telegram_event) => {
                    emit_telegram_event_with_id(&app_handle, &telegram_event, &req_short_clone);
                }
            }
        }
    });

    Ok(())
}
