use anyhow::Result;
use serde::Serialize;
// use tauri::{AppHandle, Emitter}; // 暂时不需要，由调用方处理事件
use teloxide::{
    prelude::*,
    types::{
        ChatId, InlineKeyboardButton, InlineKeyboardMarkup,
        MessageId, ParseMode,
    },
    Bot,
};

use super::markdown::process_telegram_markdown;

/// Telegram事件类型
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TelegramEvent {
    /// 选项状态变化
    OptionToggled { option: String, selected: bool },
    /// 文本输入更新
    TextUpdated { text: String },
    /// 继续按钮点击
    ContinuePressed,
    /// 发送按钮点击
    SendPressed,
}

/// Telegram Bot 核心功能
pub struct TelegramCore {
    pub bot: Bot,
    pub chat_id: ChatId,
}

impl TelegramCore {
    /// 创建新的Telegram核心实例
    pub fn new(bot_token: String, chat_id: String) -> Result<Self> {
        Self::new_with_api_url(bot_token, chat_id, None)
    }

    /// 创建新的Telegram核心实例，支持自定义API URL
    pub fn new_with_api_url(bot_token: String, chat_id: String, api_url: Option<String>) -> Result<Self> {
        let mut bot = Bot::new(bot_token);

        // 如果提供了自定义API URL，则设置它
        if let Some(url_str) = api_url {
            let url = reqwest::Url::parse(&url_str)
                .map_err(|e| anyhow::anyhow!("无效的API URL格式: {}", e))?;
            bot = bot.set_api_url(url);
        }

        // 解析chat_id
        let chat_id = if chat_id.starts_with('@') {
            return Err(anyhow::anyhow!("暂不支持@username格式，请使用数字Chat ID"));
        } else {
            let id = chat_id
                .parse::<i64>()
                .map_err(|_| anyhow::anyhow!("无效的Chat ID格式，请使用数字ID"))?;
            ChatId(id)
        };

        Ok(Self { bot, chat_id })
    }

    /// 发送普通消息
    pub async fn send_message(&self, message: &str) -> Result<()> {
        self.send_message_with_markdown(message, false).await
    }

    /// 发送支持Markdown的消息
    pub async fn send_message_with_markdown(
        &self,
        message: &str,
        use_markdown: bool,
    ) -> Result<()> {
        let mut send_request = self.bot.send_message(self.chat_id, message);

        // 如果启用Markdown，设置解析模式
        if use_markdown {
            send_request = send_request.parse_mode(ParseMode::MarkdownV2);
        }

        send_request
            .await
            .map_err(|e| anyhow::anyhow!("发送消息失败: {}", e))?;

        Ok(())
    }

    /// 发送选项消息（含选项按钮 + 操作按钮）
    pub async fn send_options_message(
        &self,
        message: &str,
        predefined_options: &[String],
        is_markdown: bool,
        client_name: Option<&str>,
        continue_reply_enabled: bool,
    ) -> Result<()> {
        // 构建消息内容（含客户端来源标识）
        let full_message = match client_name {
            Some(name) if !name.is_empty() => format!("[{}]\n\n{}", name, message),
            _ => message.to_string(),
        };

        // 处理消息内容
        let processed_message = if is_markdown {
            process_telegram_markdown(&full_message)
        } else {
            full_message
        };

        // 创建消息发送请求
        let mut send_request = self.bot.send_message(self.chat_id, processed_message);

        // 创建包含选项 + 操作按钮的 inline keyboard
        let inline_keyboard = Self::create_inline_keyboard(predefined_options, &[], continue_reply_enabled)?;
        send_request = send_request.reply_markup(inline_keyboard);

        // 如果是Markdown，设置解析模式
        if is_markdown {
            send_request = send_request.parse_mode(ParseMode::MarkdownV2);
        }

        match send_request.await {
            Ok(_) => Ok(()),
            Err(e) => {
                let error_str = e.to_string();

                // 检查是否是JSON解析错误但消息实际发送成功
                let has_parsing_json = error_str.contains("parsing JSON");
                let has_ok_true = error_str.contains("\\\"ok\\\":true");

                if has_parsing_json && has_ok_true {
                    // 消息实际发送成功
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("发送选项消息失败: {}", e))
                }
            }
        }
    }

    /// 创建 inline keyboard（选项按钮 + 操作按钮）
    pub fn create_inline_keyboard(
        predefined_options: &[String],
        selected_options: &[String],
        continue_reply_enabled: bool,
    ) -> Result<InlineKeyboardMarkup> {
        let mut keyboard_rows = Vec::new();

        // 添加选项按钮（每行最多2个）
        for (chunk_idx, chunk) in predefined_options.chunks(2).enumerate() {
            let mut row = Vec::new();
            for (i, option) in chunk.iter().enumerate() {
                let option_index = chunk_idx * 2 + i;
                // 使用索引作为 callback_data，避免超过 Telegram 64 字节限制
                let callback_data = format!("t:{}", option_index);
                // 根据选中状态显示按钮
                let button_text = if selected_options.contains(option) {
                    format!("✅ {}", option)
                } else {
                    option.to_string()
                };

                row.push(InlineKeyboardButton::callback(button_text, callback_data));
            }
            keyboard_rows.push(row);
        }

        // 添加操作按钮行（发送 / 继续）
        let mut action_row = Vec::new();
        if continue_reply_enabled {
            action_row.push(InlineKeyboardButton::callback("⏩ 继续", "action:continue"));
        }
        action_row.push(InlineKeyboardButton::callback("↗️ 发送", "action:send"));
        keyboard_rows.push(action_row);

        let keyboard = InlineKeyboardMarkup::new(keyboard_rows);
        Ok(keyboard)
    }

    /// 更新inline keyboard中的选项状态
    pub async fn update_inline_keyboard(
        &self,
        message_id: i32,
        predefined_options: &[String],
        selected_options: &[String],
        continue_reply_enabled: bool,
    ) -> Result<()> {
        let new_keyboard = Self::create_inline_keyboard(predefined_options, selected_options, continue_reply_enabled)?;

        match self
            .bot
            .edit_message_reply_markup(self.chat_id, MessageId(message_id))
            .reply_markup(new_keyboard)
            .await
        {
            Ok(_) => Ok(()),
            Err(_) => {
                // 键盘更新失败通常不是致命错误，记录但不中断流程
                Ok(())
            }
        }
    }
}

/// Callback query 处理结果
pub enum CallbackAction {
    /// 选项被切换
    OptionToggled(String),
    /// 操作按钮
    ActionEvent(ActionEvent),
}

/// 操作按钮事件
pub enum ActionEvent {
    Send,
    Continue,
}

/// 处理callback query的通用函数
/// 返回 CallbackAction 或 None
pub async fn handle_callback_query(
    bot: &Bot,
    callback_query: &CallbackQuery,
    target_chat_id: ChatId,
    predefined_options: &[String],
) -> ResponseResult<Option<CallbackAction>> {
    // 检查是否是目标聊天
    if let Some(message) = &callback_query.message {
        if message.chat().id != target_chat_id {
            return Ok(None);
        }
    }

    let mut toggled_option = None;

    if let Some(data) = &callback_query.data {
        // 操作按钮: action:send / action:continue
        if data == "action:send" {
            bot.answer_callback_query(&callback_query.id).await?;
            return Ok(Some(CallbackAction::ActionEvent(ActionEvent::Send)));
        } else if data == "action:continue" {
            bot.answer_callback_query(&callback_query.id).await?;
            return Ok(Some(CallbackAction::ActionEvent(ActionEvent::Continue)));
        }

        // 选项按钮 - 新格式: t:索引
        if let Some(index_str) = data.strip_prefix("t:") {
            if let Ok(index) = index_str.parse::<usize>() {
                if let Some(option) = predefined_options.get(index) {
                    toggled_option = Some(option.clone());
                }
            }
        }
        // 兼容旧格式: toggle:选项文本（校验选项合法性）
        else if let Some(option) = data.strip_prefix("toggle:") {
            if predefined_options.contains(&option.to_string()) {
                toggled_option = Some(option.to_string());
            }
        }
    }

    // 回答callback query
    bot.answer_callback_query(&callback_query.id).await?;

    Ok(toggled_option.map(CallbackAction::OptionToggled))
}

/// 处理文本消息的通用函数（不发送事件，由调用方处理）
pub async fn handle_text_message(
    message: &Message,
    target_chat_id: ChatId,
    operation_message_id: Option<i32>,
) -> ResponseResult<Option<TelegramEvent>> {
    // 检查是否是目标聊天
    if message.chat.id != target_chat_id {
        return Ok(None);
    }

    // 检查消息ID过滤
    if let Some(op_id) = operation_message_id {
        if message.id.0 <= op_id {
            return Ok(None);
        }
    }

    if let Some(text) = message.text() {
        let event = match text {
            "⏩继续" | "/heng_continue" | "/heng-continue" => TelegramEvent::ContinuePressed,
            "↗️发送" | "/heng_send" | "/heng-send" => TelegramEvent::SendPressed,
            _ => TelegramEvent::TextUpdated {
                text: text.to_string(),
            },
        };

        return Ok(Some(event));
    }

    Ok(None)
}

/// 生成统一的反馈消息
pub fn build_feedback_message(
    selected_options: &[String],
    user_input: &str,
    is_continue: bool,
) -> String {
    if is_continue {
        // 继续操作的反馈消息
        let continue_prompt = if let Ok(config) = crate::config::load_standalone_config() {
            config.reply_config.continue_prompt
        } else {
            "请按照最佳实践继续".to_string()
        };

        format!("✅ 发送成功！\n\n📝 选中的选项：\n• ⏩ {}", continue_prompt)
    } else {
        // 发送操作的反馈消息
        let mut feedback_message = "✅ 发送成功！\n\n📝 选中的选项：\n".to_string();

        if selected_options.is_empty() {
            feedback_message.push_str("• 无");
        } else {
            for opt in selected_options {
                feedback_message.push_str(&format!("• {}\n", opt));
            }
        }

        if !user_input.is_empty() {
            feedback_message.push_str(&format!("\n📝 补充说明：\n{}", user_input));
        }

        feedback_message
    }
}

/// 测试Telegram连接的通用函数
pub async fn test_telegram_connection(bot_token: &str, chat_id: &str) -> Result<String> {
    test_telegram_connection_with_api_url(bot_token, chat_id, None).await
}

/// 测试Telegram连接的通用函数，支持自定义API URL
pub async fn test_telegram_connection_with_api_url(
    bot_token: &str,
    chat_id: &str,
    api_url: Option<&str>
) -> Result<String> {
    if bot_token.trim().is_empty() {
        return Err(anyhow::anyhow!("Bot Token不能为空"));
    }

    if chat_id.trim().is_empty() {
        return Err(anyhow::anyhow!("Chat ID不能为空"));
    }

    // 创建Bot实例
    let mut bot = Bot::new(bot_token);

    // 如果提供了自定义API URL，则设置它
    if let Some(url_str) = api_url {
        let url = reqwest::Url::parse(url_str)
            .map_err(|e| anyhow::anyhow!("无效的API URL格式: {}", e))?;
        bot = bot.set_api_url(url);
    }

    // 验证Chat ID格式
    let chat_id_parsed: i64 = chat_id
        .parse()
        .map_err(|_| anyhow::anyhow!("Chat ID格式无效，请输入有效的数字ID"))?;

    // 发送测试消息
    let test_message =
        "🤖 恒境应用测试消息\n\n这是一条来自恒境应用的测试消息，表示Telegram Bot配置成功！";

    match bot.send_message(ChatId(chat_id_parsed), test_message).await {
        Ok(_) => Ok("测试消息发送成功！Telegram Bot配置正确。".to_string()),
        Err(e) => Err(anyhow::anyhow!("发送测试消息失败: {}", e)),
    }
}
