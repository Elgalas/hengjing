use anyhow::Result;

use crate::config::load_standalone_config;
use crate::mcp::types::{build_continue_response, build_send_response, PopupRequest};
use crate::telegram::{
    DispatcherConfig, SessionAction, SessionEvent,
    get_or_init_dispatcher,
};
use crate::log_important;

/// 处理纯Telegram模式的MCP请求（不启动GUI）
pub async fn handle_telegram_only_mcp_request(request_file: &str) -> Result<()> {
    // 读取MCP请求文件
    let request_json = std::fs::read_to_string(request_file)?;
    let request: PopupRequest = serde_json::from_str(&request_json)?;

    // 加载完整配置
    let app_config = load_standalone_config()?;
    let telegram_config = &app_config.telegram_config;

    if !telegram_config.enabled {
        log_important!(warn, "Telegram未启用，无法处理请求");
        return Ok(());
    }

    if telegram_config.bot_token.trim().is_empty() || telegram_config.chat_id.trim().is_empty() {
        log_important!(warn, "Telegram配置不完整");
        return Ok(());
    }

    // 准备 API URL
    let api_url = if telegram_config.api_base_url == crate::constants::telegram::API_BASE_URL {
        None
    } else {
        Some(telegram_config.api_base_url.clone())
    };

    // 获取或初始化全局调度器
    let dispatcher = get_or_init_dispatcher(DispatcherConfig {
        bot_token: telegram_config.bot_token.clone(),
        chat_id: telegram_config.chat_id.clone(),
        api_url,
    }).await?;

    let predefined_options = request.predefined_options.clone().unwrap_or_default();
    let continue_reply_enabled = app_config.reply_config.enable_continue_reply;

    // 注册会话
    let (mut event_rx, req_short, seq_num) = dispatcher
        .register_session(
            request.id.clone(),
            predefined_options.clone(),
            continue_reply_enabled,
        )
        .await;

    // 发送选项消息到 Telegram
    let send_result = dispatcher
        .send_options_message(
            &req_short,
            seq_num,
            &request.message,
            &predefined_options,
            request.is_markdown,
            request.client_name.as_deref(),
            continue_reply_enabled,
        )
        .await;

    if let Err(e) = send_result {
        // 发送失败，回滚会话注册
        dispatcher.unregister_session(&req_short).await;
        return Err(e);
    }

    // 等待事件（CLI 模式是阻塞式的）
    while let Some(event) = event_rx.recv().await {
        match event {
            SessionEvent::Action(SessionAction::Send) => {
                // 获取选中选项和用户输入
                let (selected_list, user_input) = dispatcher
                    .get_session_state(&req_short)
                    .await
                    .unwrap_or_default();

                let user_input_option = if user_input.is_empty() {
                    None
                } else {
                    Some(user_input.clone())
                };

                let response = build_send_response(
                    user_input_option,
                    selected_list.clone(),
                    vec![], // 无GUI模式下没有图片
                    Some(request.id.clone()),
                    "telegram",
                );

                // 输出JSON响应到stdout（MCP协议要求）
                println!("{}", response);

                // 发送确认消息
                let feedback = crate::telegram::core::build_feedback_message(
                    &selected_list, &user_input, false,
                );
                let _ = dispatcher.send_message(&feedback).await;

                // 注销会话
                dispatcher.unregister_session(&req_short).await;
                return Ok(());
            }
            SessionEvent::Action(SessionAction::Continue) => {
                let response = build_continue_response(
                    Some(request.id.clone()),
                    "telegram_continue",
                );

                // 输出JSON响应到stdout
                println!("{}", response);

                // 发送确认消息
                let feedback = crate::telegram::core::build_feedback_message(
                    &[], "", true,
                );
                let _ = dispatcher.send_message(&feedback).await;

                // 注销会话
                dispatcher.unregister_session(&req_short).await;
                return Ok(());
            }
            SessionEvent::Telegram(_) => {
                // CLI 模式下 Telegram 事件（选项切换、文本更新）
                // 已由 dispatcher 内部管理状态，无需额外处理
            }
        }
    }

    Ok(())
}
