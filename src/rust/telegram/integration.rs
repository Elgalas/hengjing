use anyhow::Result;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use teloxide::prelude::*;
use tokio::sync::Mutex;

use super::core::{handle_callback_query, handle_text_message, CallbackAction, ActionEvent, TelegramCore, TelegramEvent};
use crate::log_important;

/// Telegram集成管理器
pub struct TelegramIntegration {
    core: TelegramCore,
    app_handle: AppHandle,
    /// 当前选中的选项
    selected_options: Arc<Mutex<Vec<String>>>,
    /// 用户输入文本
    user_input: Arc<Mutex<String>>,
    /// 预定义选项列表
    predefined_options: Vec<String>,
    /// 是否启用继续回复
    continue_reply_enabled: bool,
    /// 停止信号发送器
    stop_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TelegramIntegration {
    /// 创建新的Telegram集成实例
    pub fn new(bot_token: String, chat_id: String, app_handle: AppHandle) -> Result<Self> {
        Self::new_with_api_url(bot_token, chat_id, app_handle, None)
    }

    /// 创建新的Telegram集成实例，支持自定义API URL
    pub fn new_with_api_url(bot_token: String, chat_id: String, app_handle: AppHandle, api_url: Option<String>) -> Result<Self> {
        let core = TelegramCore::new_with_api_url(bot_token, chat_id, api_url)?;

        Ok(Self {
            core,
            app_handle,
            selected_options: Arc::new(Mutex::new(Vec::new())),
            user_input: Arc::new(Mutex::new(String::new())),
            predefined_options: Vec::new(),
            continue_reply_enabled: false,
            stop_sender: None,
        })
    }

    /// 发送MCP请求消息到Telegram
    pub async fn send_mcp_request(
        &mut self,
        message: &str,
        predefined_options: Vec<String>,
        is_markdown: bool,
        continue_reply_enabled: bool,
    ) -> Result<()> {
        // 初始化选中选项状态
        {
            let mut selected = self.selected_options.lock().await;
            selected.clear();
        }

        // 保存选项列表供监听使用
        self.predefined_options = predefined_options.clone();
        self.continue_reply_enabled = continue_reply_enabled;

        // 发送选项消息（含 Inline 操作按钮）
        self.core
            .send_options_message(message, &predefined_options, is_markdown, None, continue_reply_enabled)
            .await?;

        // 启动消息监听
        self.start_message_listener().await?;

        Ok(())
    }



    /// 启动消息监听
    async fn start_message_listener(&mut self) -> Result<()> {
        let bot = self.core.bot.clone();
        let chat_id = self.core.chat_id;
        let app_handle = self.app_handle.clone();
        let selected_options = self.selected_options.clone();
        let user_input = self.user_input.clone();
        let predefined_options = self.predefined_options.clone();
        let continue_reply_enabled = self.continue_reply_enabled;

        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        self.stop_sender = Some(stop_tx);

        // 启动监听任务
        tokio::spawn(async move {
            let mut offset = 0i32;
            let mut options_message_id: Option<i32> = None;

            // 获取当前最新的消息ID作为基准
            if let Ok(updates) = bot.get_updates().limit(10).await {
                if let Some(update) = updates.last() {
                    offset = update.id.0 as i32 + 1;
                }
            }

            loop {
                tokio::select! {
                    _ = &mut stop_rx => {
                        break;
                    }
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(1000)) => {
                        match bot.get_updates().offset(offset).timeout(10).await {
                            Ok(updates) => {
                                for update in updates {
                                    offset = update.id.0 as i32 + 1;

                                    match update.kind {
                                        teloxide::types::UpdateKind::CallbackQuery(callback_query) => {
                                            // 提取消息ID
                                            if let Some(message) = &callback_query.message {
                                                if options_message_id.is_none() {
                                                    options_message_id = Some(message.id().0);
                                                }
                                            }

                                            if let Ok(Some(action)) = handle_callback_query(
                                                &bot, &callback_query, chat_id, &predefined_options,
                                            ).await {
                                                match action {
                                                    CallbackAction::ActionEvent(ActionEvent::Send) => {
                                                        let event = TelegramEvent::SendPressed;
                                                        let _ = app_handle.emit("telegram-event", &event);
                                                    }
                                                    CallbackAction::ActionEvent(ActionEvent::Continue) => {
                                                        let event = TelegramEvent::ContinuePressed;
                                                        let _ = app_handle.emit("telegram-event", &event);
                                                    }
                                                    CallbackAction::OptionToggled(option) => {
                                                        let selected = {
                                                            let mut selected_opts = selected_options.lock().await;
                                                            if selected_opts.contains(&option) {
                                                                selected_opts.retain(|x| x != &option);
                                                                false
                                                            } else {
                                                                selected_opts.push(option.clone());
                                                                true
                                                            }
                                                        };

                                                        let event = TelegramEvent::OptionToggled {
                                                            option: option.clone(),
                                                            selected,
                                                        };
                                                        let _ = app_handle.emit("telegram-event", &event);

                                                        // 更新 inline keyboard 按钮状态
                                                        if let Some(msg_id) = options_message_id {
                                                            let selected_vec: Vec<String> = selected_options.lock().await.clone();
                                                            let core_temp = TelegramCore { bot: bot.clone(), chat_id };
                                                            let _ = core_temp.update_inline_keyboard(
                                                                msg_id, &predefined_options, &selected_vec, continue_reply_enabled,
                                                            ).await;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        teloxide::types::UpdateKind::Message(message) => {
                                            // 识别选项消息ID
                                            if let Some(inline_keyboard) = message.reply_markup() {
                                                for row in &inline_keyboard.inline_keyboard {
                                                    for button in row {
                                                        if let teloxide::types::InlineKeyboardButtonKind::CallbackData(cb) = &button.kind {
                                                            if cb.starts_with("t:") || cb.starts_with("action:") {
                                                                options_message_id = Some(message.id.0);
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            match handle_text_message(&message, chat_id, None).await {
                                                Ok(Some(event)) => {
                                                    if let TelegramEvent::TextUpdated { text } = &event {
                                                        let mut input = user_input.lock().await;
                                                        *input = text.clone();
                                                    }
                                                    let _ = app_handle.emit("telegram-event", &event);
                                                }
                                                Ok(None) => {}
                                                Err(e) => {
                                                    log_important!(warn, "文本消息处理失败: {}", e);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Err(_e) => {
                                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// 获取当前选中的选项
    pub async fn get_selected_options(&self) -> Vec<String> {
        let selected = self.selected_options.lock().await;
        selected.clone()
    }

    /// 获取用户输入的文本
    pub async fn get_user_input(&self) -> String {
        let input = self.user_input.lock().await;
        input.clone()
    }

    /// 停止Telegram集成
    pub async fn stop(&mut self) {
        if let Some(sender) = self.stop_sender.take() {
            let _ = sender.send(());
        }
    }
}


