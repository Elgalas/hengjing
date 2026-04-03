//! Telegram 中心化 Polling 调度器
//!
//! 全局唯一的 polling loop，收到 update 后根据 request_id 路由到对应会话。
//! 解决多请求并行时 get_updates 竞争和事件交叉问题。

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::{Mutex, mpsc, oneshot};

use teloxide::prelude::*;
use teloxide::types::{ChatId, MessageId};

use super::core::{TelegramCore, TelegramEvent};
use crate::log_important;

/// 全局调度器单例（支持配置变更时重建）
static DISPATCHER: tokio::sync::OnceCell<tokio::sync::RwLock<Option<DispatcherInstance>>> =
    tokio::sync::OnceCell::const_new();

/// 调度器实例（含配置指纹）
struct DispatcherInstance {
    dispatcher: TelegramDispatcher,
    config_fingerprint: String,
}

/// 生成配置指纹
fn config_fingerprint(config: &DispatcherConfig) -> String {
    format!(
        "{}:{}:{}",
        config.bot_token,
        config.chat_id,
        config.api_url.as_deref().unwrap_or("")
    )
}

/// 获取或初始化全局调度器
/// 如果配置已变更，自动停止旧实例并重建
pub async fn get_or_init_dispatcher(config: DispatcherConfig) -> Result<&'static TelegramDispatcher> {
    let lock = DISPATCHER
        .get_or_init(|| async { tokio::sync::RwLock::new(None) })
        .await;

    let fingerprint = config_fingerprint(&config);

    // 快速路径：读锁检查现有实例
    {
        let read_guard = lock.read().await;
        if let Some(instance) = read_guard.as_ref() {
            if instance.config_fingerprint == fingerprint {
                // 配置一致，返回现有实例
                // SAFETY: 实例生命周期与 static DISPATCHER 绑定
                let ptr = &instance.dispatcher as *const TelegramDispatcher;
                return Ok(unsafe { &*ptr });
            }
        }
    }

    // 慢速路径：需要创建或重建
    let mut write_guard = lock.write().await;

    // 双重检查（避免写锁等待期间被其他线程初始化）
    if let Some(instance) = write_guard.as_ref() {
        if instance.config_fingerprint == fingerprint {
            let ptr = &instance.dispatcher as *const TelegramDispatcher;
            return Ok(unsafe { &*ptr });
        }
        // 配置不同，停止旧 polling
        log_important!(info, "Telegram 配置已变更，重建调度器");
        instance.dispatcher.stop_polling().await;
    }

    // 创建新实例
    let dispatcher = TelegramDispatcher::new(config)?;
    *write_guard = Some(DispatcherInstance {
        dispatcher,
        config_fingerprint: fingerprint,
    });

    let ptr = &write_guard.as_ref().unwrap().dispatcher as *const TelegramDispatcher;
    Ok(unsafe { &*ptr })
}

/// 获取已初始化的全局调度器（如果存在）
pub async fn get_dispatcher_async() -> Option<&'static TelegramDispatcher> {
    let lock = DISPATCHER.get()?;
    let read_guard = lock.read().await;
    read_guard.as_ref().map(|instance| {
        let ptr = &instance.dispatcher as *const TelegramDispatcher;
        unsafe { &*ptr }
    })
}

/// 会话事件（调度器发给每个请求会话的）
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Telegram 事件
    Telegram(TelegramEvent),
    /// 操作事件（发送/继续）
    Action(SessionAction),
}

/// 操作事件
#[derive(Debug, Clone)]
pub enum SessionAction {
    Send,
    Continue,
}

/// 请求会话信息
struct RequestSession {
    /// request_id 简短标识（前 6 位）
    request_id_short: String,
    /// 该请求在 Telegram 上的消息 ID
    telegram_message_id: Option<i32>,
    /// 预定义选项
    predefined_options: Vec<String>,
    /// 是否启用继续回复
    continue_reply_enabled: bool,
    /// 当前选中的选项
    selected_options: std::collections::HashSet<String>,
    /// 用户输入文本
    user_input: String,
    /// 事件发送通道
    event_tx: mpsc::UnboundedSender<SessionEvent>,
}

/// Telegram 调度器配置
pub struct DispatcherConfig {
    pub bot_token: String,
    pub chat_id: String,
    pub api_url: Option<String>,
}

/// Telegram 中心化调度器
pub struct TelegramDispatcher {
    /// Telegram 核心实例
    core: TelegramCore,
    /// 活跃的请求会话 (request_id_short -> session)
    sessions: Arc<Mutex<HashMap<String, RequestSession>>>,
    /// 会话序号计数器
    sequence_counter: Arc<AtomicU32>,
    /// 是否正在运行 polling
    polling_active: Arc<Mutex<bool>>,
    /// polling task 的取消信号
    polling_cancel: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// polling task 的 JoinHandle（用于等待退出）
    polling_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl TelegramDispatcher {
    /// 创建新的调度器
    pub fn new(config: DispatcherConfig) -> Result<Self> {
        let core = TelegramCore::new_with_api_url(
            config.bot_token,
            config.chat_id,
            config.api_url,
        )?;

        Ok(Self {
            core,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            sequence_counter: Arc::new(AtomicU32::new(1)),
            polling_active: Arc::new(Mutex::new(false)),
            polling_cancel: Arc::new(Mutex::new(None)),
            polling_handle: Arc::new(Mutex::new(None)),
        })
    }

    /// 获取 TelegramCore 引用（用于发送消息等）
    pub fn core(&self) -> &TelegramCore {
        &self.core
    }

    /// 注册一个新的请求会话
    /// 返回 (event_rx, request_id_short, sequence_number)
    pub async fn register_session(
        &self,
        request_id: String,
        predefined_options: Vec<String>,
        continue_reply_enabled: bool,
    ) -> (mpsc::UnboundedReceiver<SessionEvent>, String, u32) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // 生成 request_id_short，确保不与现有会话碰撞
        let request_id_short = {
            let sessions = self.sessions.lock().await;
            let mut short_len = 6.min(request_id.len());
            loop {
                let candidate = request_id[..short_len].to_string();
                if !sessions.contains_key(&candidate) {
                    break candidate;
                }
                // 碰撞，扩展长度
                short_len += 2;
                if short_len >= request_id.len() {
                    break request_id.clone();
                }
            }
        };

        let sequence_number = self.sequence_counter.fetch_add(1, Ordering::Relaxed);

        let session = RequestSession {
            request_id_short: request_id_short.clone(),
            telegram_message_id: None,
            predefined_options,
            continue_reply_enabled,
            selected_options: std::collections::HashSet::new(),
            user_input: String::new(),
            event_tx,
        };

        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(request_id_short.clone(), session);
        }

        // 确保 polling 正在运行
        self.ensure_polling().await;

        (event_rx, request_id_short, sequence_number)
    }

    /// 设置会话的 Telegram 消息 ID（发送消息后调用）
    pub async fn set_session_message_id(
        &self,
        request_id_short: &str,
        message_id: i32,
    ) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(request_id_short) {
            session.telegram_message_id = Some(message_id);
        }
    }

    /// 注销请求会话
    pub async fn unregister_session(&self, request_id_short: &str) {
        let mut sessions = self.sessions.lock().await;
        sessions.remove(request_id_short);

        // 如果没有活跃会话了，停止 polling
        if sessions.is_empty() {
            drop(sessions);
            self.stop_polling().await;
        }
    }

    /// 获取活跃会话数量
    pub async fn active_session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// 发送选项消息到 Telegram，返回 message_id
    pub async fn send_options_message(
        &self,
        request_id_short: &str,
        sequence_number: u32,
        message: &str,
        predefined_options: &[String],
        is_markdown: bool,
        client_name: Option<&str>,
        continue_reply_enabled: bool,
    ) -> Result<i32> {
        // 构建消息内容（含序号标签和客户端来源标识）
        let session_count = self.active_session_count().await;
        let full_message = {
            let mut msg = String::new();
            // 多请求时添加序号标签
            if session_count > 1 {
                msg.push_str(&format!("[#{}] ", sequence_number));
            }
            // 客户端来源标识
            if let Some(name) = client_name {
                if !name.is_empty() {
                    msg.push_str(&format!("[{}]\n\n", name));
                }
            }
            msg.push_str(message);
            msg
        };

        // 处理消息内容
        let processed_message = if is_markdown {
            super::markdown::process_telegram_markdown(&full_message)
        } else {
            full_message
        };

        // 创建带 request_id_short 的 inline keyboard
        let inline_keyboard = create_inline_keyboard_with_id(
            request_id_short,
            predefined_options,
            &[],
            continue_reply_enabled,
        );

        // 发送消息
        let mut send_request = self.core.bot.send_message(self.core.chat_id, processed_message);
        send_request = send_request.reply_markup(inline_keyboard);

        if is_markdown {
            send_request = send_request.parse_mode(teloxide::types::ParseMode::MarkdownV2);
        }

        match send_request.await {
            Ok(msg) => {
                let msg_id = msg.id.0;
                self.set_session_message_id(request_id_short, msg_id).await;
                Ok(msg_id)
            }
            Err(e) => {
                let error_str = e.to_string();
                if error_str.contains("parsing JSON") && error_str.contains("\\\"ok\\\":true") {
                    Ok(0)
                } else {
                    Err(anyhow::anyhow!("发送选项消息失败: {}", e))
                }
            }
        }
    }

    /// 确保 polling loop 正在运行
    async fn ensure_polling(&self) {
        let mut active = self.polling_active.lock().await;
        if *active {
            // 检查旧 handle 是否还活着
            let mut handle_guard = self.polling_handle.lock().await;
            if let Some(handle) = handle_guard.as_ref() {
                if !handle.is_finished() {
                    return; // polling 确实在运行
                }
                // handle 已结束但 active 未重置（不应发生，防御性处理）
                handle_guard.take();
            }
            // 旧 polling 已退出，重置标志
            *active = false;
        }
        *active = true;
        drop(active);

        let (cancel_tx, cancel_rx) = oneshot::channel();
        {
            let mut cancel = self.polling_cancel.lock().await;
            *cancel = Some(cancel_tx);
        }

        let bot = self.core.bot.clone();
        let chat_id = self.core.chat_id;
        let sessions = self.sessions.clone();
        let polling_active = self.polling_active.clone();

        let handle = tokio::spawn(async move {
            polling_loop(bot, chat_id, sessions, cancel_rx).await;
            let mut active = polling_active.lock().await;
            *active = false;
        });

        {
            let mut handle_guard = self.polling_handle.lock().await;
            *handle_guard = Some(handle);
        }
    }

    /// 停止 polling（发送取消信号并等待退出）
    pub(crate) async fn stop_polling(&self) {
        // 发送取消信号
        {
            let mut cancel = self.polling_cancel.lock().await;
            if let Some(tx) = cancel.take() {
                let _ = tx.send(());
            }
        }
        // 等待 polling task 退出
        let handle = {
            let mut handle_guard = self.polling_handle.lock().await;
            handle_guard.take()
        };
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }
}

/// 中心化 polling 循环
async fn polling_loop(
    bot: Bot,
    chat_id: ChatId,
    sessions: Arc<Mutex<HashMap<String, RequestSession>>>,
    mut cancel_rx: oneshot::Receiver<()>,
) {
    let mut offset = 0i32;

    // 获取当前最新的消息ID作为基准
    if let Ok(updates) = bot.get_updates().limit(10).await {
        if let Some(update) = updates.last() {
            offset = update.id.0 as i32 + 1;
        }
    }

    loop {
        tokio::select! {
            _ = &mut cancel_rx => {
                log_important!(info, "Telegram polling 已停止");
                break;
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(1000)) => {
                match bot.get_updates().offset(offset).timeout(10).await {
                    Ok(updates) => {
                        for update in updates {
                            offset = update.id.0 as i32 + 1;

                            match update.kind {
                                teloxide::types::UpdateKind::CallbackQuery(callback_query) => {
                                    handle_callback_dispatch(
                                        &bot, &callback_query, chat_id, &sessions,
                                    ).await;
                                }
                                teloxide::types::UpdateKind::Message(message) => {
                                    handle_message_dispatch(
                                        &bot, &message, chat_id, &sessions,
                                    ).await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(_) => {
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                }
            }
        }
    }
}

/// 处理 callback query 分发
async fn handle_callback_dispatch(
    bot: &Bot,
    callback_query: &teloxide::types::CallbackQuery,
    chat_id: ChatId,
    sessions: &Arc<Mutex<HashMap<String, RequestSession>>>,
) {
    // 检查是否是目标聊天
    if let Some(message) = &callback_query.message {
        if message.chat().id != chat_id {
            return;
        }
    }

    let Some(data) = &callback_query.data else {
        let _ = bot.answer_callback_query(&callback_query.id).await;
        return;
    };

    // 解析 callback_data 中的 request_id_short
    // 格式: t:req_short:index / a:req_short:s / a:req_short:c
    let parts: Vec<&str> = data.splitn(3, ':').collect();
    if parts.len() < 3 {
        // 兼容旧格式（无 request_id）：尝试路由到唯一活跃会话
        handle_legacy_callback(bot, callback_query, chat_id, data, sessions).await;
        return;
    }

    let prefix = parts[0];
    let req_short = parts[1];
    let payload = parts[2];

    let mut sessions_guard = sessions.lock().await;

    let Some(session) = sessions_guard.get_mut(req_short) else {
        // 会话不存在（可能已完成）
        let _ = bot.answer_callback_query(&callback_query.id).await;
        return;
    };

    // 记录 telegram_message_id
    if session.telegram_message_id.is_none() {
        if let Some(message) = &callback_query.message {
            session.telegram_message_id = Some(message.id().0);
        }
    }

    match prefix {
        "a" => {
            // 操作按钮
            let _ = bot.answer_callback_query(&callback_query.id).await;
            let action = match payload {
                "s" => SessionAction::Send,
                "c" => SessionAction::Continue,
                _ => return,
            };
            let _ = session.event_tx.send(SessionEvent::Action(action));
        }
        "t" => {
            // 选项按钮
            let _ = bot.answer_callback_query(&callback_query.id).await;
            if let Ok(index) = payload.parse::<usize>() {
                if let Some(option) = session.predefined_options.get(index).cloned() {
                    let selected = if session.selected_options.contains(&option) {
                        session.selected_options.remove(&option);
                        false
                    } else {
                        session.selected_options.insert(option.clone());
                        true
                    };

                    let _ = session.event_tx.send(SessionEvent::Telegram(
                        TelegramEvent::OptionToggled {
                            option: option.clone(),
                            selected,
                        },
                    ));

                    // 更新 inline keyboard
                    if let Some(msg_id) = session.telegram_message_id {
                        let selected_vec: Vec<String> =
                            session.selected_options.iter().cloned().collect();
                        let new_keyboard = create_inline_keyboard_with_id(
                            &session.request_id_short,
                            &session.predefined_options,
                            &selected_vec,
                            session.continue_reply_enabled,
                        );
                        drop(sessions_guard);
                        let _ = bot
                            .edit_message_reply_markup(chat_id, MessageId(msg_id))
                            .reply_markup(new_keyboard)
                            .await;
                    }
                }
            }
        }
        _ => {
            let _ = bot.answer_callback_query(&callback_query.id).await;
        }
    }
}

/// 处理旧格式 callback（无 request_id）
async fn handle_legacy_callback(
    bot: &Bot,
    callback_query: &teloxide::types::CallbackQuery,
    _chat_id: ChatId,
    data: &str,
    sessions: &Arc<Mutex<HashMap<String, RequestSession>>>,
) {
    let mut sessions_guard = sessions.lock().await;

    // 尝试根据 message_id 匹配或路由到唯一会话
    let session_key = if sessions_guard.len() == 1 {
        sessions_guard.keys().next().cloned()
    } else {
        // 多会话时尝试根据 message_id 匹配
        let msg_id = callback_query.message.as_ref().map(|m| m.id().0);
        sessions_guard.iter().find_map(|(key, session)| {
            if session.telegram_message_id == msg_id {
                Some(key.clone())
            } else {
                None
            }
        })
    };

    let _ = bot.answer_callback_query(&callback_query.id).await;

    let Some(key) = session_key else { return };
    let Some(session) = sessions_guard.get_mut(&key) else { return };

    // 兼容旧 toggle: 格式
    if let Some(option) = data.strip_prefix("toggle:") {
        if session.predefined_options.contains(&option.to_string()) {
            let selected = if session.selected_options.contains(option) {
                session.selected_options.remove(option);
                false
            } else {
                session.selected_options.insert(option.to_string());
                true
            };
            let _ = session.event_tx.send(SessionEvent::Telegram(
                TelegramEvent::OptionToggled {
                    option: option.to_string(),
                    selected,
                },
            ));
        }
    }
    // 兼容旧 t:index 格式（无 request_id）
    else if let Some(index_str) = data.strip_prefix("t:") {
        if let Ok(index) = index_str.parse::<usize>() {
            if let Some(option) = session.predefined_options.get(index).cloned() {
                let selected = if session.selected_options.contains(&option) {
                    session.selected_options.remove(&option);
                    false
                } else {
                    session.selected_options.insert(option.clone());
                    true
                };
                let _ = session.event_tx.send(SessionEvent::Telegram(
                    TelegramEvent::OptionToggled {
                        option,
                        selected,
                    },
                ));
            }
        }
    }
    // 兼容旧 action: 格式
    else if data == "action:send" {
        let _ = session.event_tx.send(SessionEvent::Action(SessionAction::Send));
    } else if data == "action:continue" {
        let _ = session.event_tx.send(SessionEvent::Action(SessionAction::Continue));
    }
}

/// 处理文本消息分发
async fn handle_message_dispatch(
    bot: &Bot,
    message: &teloxide::types::Message,
    chat_id: ChatId,
    sessions: &Arc<Mutex<HashMap<String, RequestSession>>>,
) {
    // 检查是否是目标聊天
    if message.chat.id != chat_id {
        return;
    }

    let sessions_guard = sessions.lock().await;
    let session_count = sessions_guard.len();

    if session_count == 0 {
        return;
    }

    // 确定目标会话
    let target_key = if session_count == 1 {
        // 单请求模式：直接路由
        sessions_guard.keys().next().cloned()
    } else {
        // 多请求模式：检查 reply_to_message
        if let Some(reply_msg) = message.reply_to_message() {
            let reply_msg_id = reply_msg.id.0;
            sessions_guard.iter().find_map(|(key, session)| {
                if session.telegram_message_id == Some(reply_msg_id) {
                    Some(key.clone())
                } else {
                    None
                }
            })
        } else {
            // 无 reply，在多请求模式下拒绝
            None
        }
    };

    drop(sessions_guard);

    let Some(target_key) = target_key else {
        // 多请求模式下无法路由 — 提示用户使用 reply
        if session_count > 1 {
            if let Some(text) = message.text() {
                // 避免对非相关消息做提示
                if !text.is_empty() {
                    let hint = format!(
                        "当前有 {} 个活跃请求，请使用引用回复(reply)指定目标请求。",
                        session_count
                    );
                    let _ = bot.send_message(chat_id, hint).await;
                }
            }
        }
        return;
    };

    // 处理文本消息
    let Some(text) = message.text() else { return };

    let event = match text {
        "⏩继续" | "/heng_continue" | "/heng-continue" => {
            SessionEvent::Action(SessionAction::Continue)
        }
        "↗️发送" | "/heng_send" | "/heng-send" => {
            SessionEvent::Action(SessionAction::Send)
        }
        _ => {
            // 保存用户输入
            let mut sessions_guard = sessions.lock().await;
            if let Some(session) = sessions_guard.get_mut(&target_key) {
                session.user_input = text.to_string();
            }
            SessionEvent::Telegram(TelegramEvent::TextUpdated {
                text: text.to_string(),
            })
        }
    };

    let sessions_guard = sessions.lock().await;
    if let Some(session) = sessions_guard.get(&target_key) {
        let _ = session.event_tx.send(event);
    }
}

impl TelegramDispatcher {
    /// 获取会话的选中选项和用户输入
    pub async fn get_session_state(
        &self,
        request_id_short: &str,
    ) -> Option<(Vec<String>, String)> {
        let sessions_guard = self.sessions.lock().await;
        sessions_guard.get(request_id_short).map(|session| {
            let selected: Vec<String> = session.selected_options.iter().cloned().collect();
            let user_input = session.user_input.clone();
            (selected, user_input)
        })
    }

    /// 发送普通消息
    pub async fn send_message(&self, message: &str) -> Result<()> {
        self.core.send_message(message).await
    }
}

/// 创建带 request_id_short 的 inline keyboard
pub fn create_inline_keyboard_with_id(
    request_id_short: &str,
    predefined_options: &[String],
    selected_options: &[String],
    continue_reply_enabled: bool,
) -> teloxide::types::InlineKeyboardMarkup {
    use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

    let mut keyboard_rows = Vec::new();

    // 添加选项按钮（每行最多2个）
    for (chunk_idx, chunk) in predefined_options.chunks(2).enumerate() {
        let mut row = Vec::new();
        for (i, option) in chunk.iter().enumerate() {
            let option_index = chunk_idx * 2 + i;
            // 格式: t:req_short:index
            let callback_data = format!("t:{}:{}", request_id_short, option_index);
            let button_text = if selected_options.contains(option) {
                format!("✅ {}", option)
            } else {
                option.to_string()
            };
            row.push(InlineKeyboardButton::callback(button_text, callback_data));
        }
        keyboard_rows.push(row);
    }

    // 添加操作按钮行
    let mut action_row = Vec::new();
    if continue_reply_enabled {
        // 格式: a:req_short:c
        action_row.push(InlineKeyboardButton::callback(
            "⏩ 继续",
            format!("a:{}:c", request_id_short),
        ));
    }
    // 格式: a:req_short:s
    action_row.push(InlineKeyboardButton::callback(
        "↗️ 发送",
        format!("a:{}:s", request_id_short),
    ));
    keyboard_rows.push(action_row);

    InlineKeyboardMarkup::new(keyboard_rows)
}
