//! IPC 服务器
//!
//! 在 UI 进程中运行，监听来自 MCP 的请求

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

use super::{get_socket_path, IpcRequest, IpcResponse};
use crate::log_important;

/// 等待中的请求
pub struct PendingRequest {
    pub request: IpcRequest,
    pub response_tx: oneshot::Sender<String>,
}

/// IPC 服务器状态
pub struct IpcServerState {
    /// 等待响应的请求（按 request_id 索引）
    pending_requests: Mutex<HashMap<String, PendingRequest>>,
    /// 新请求通道发送端
    request_tx: mpsc::Sender<IpcRequest>,
    /// 客户端断开通知通道（发送 request_id）
    disconnect_tx: mpsc::Sender<String>,
}

impl IpcServerState {
    pub fn new(request_tx: mpsc::Sender<IpcRequest>, disconnect_tx: mpsc::Sender<String>) -> Self {
        Self {
            pending_requests: Mutex::new(HashMap::new()),
            request_tx,
            disconnect_tx,
        }
    }

    /// 设置新的挂起请求，立即返回 oneshot::Receiver
    pub async fn set_pending(&self, request: IpcRequest) -> oneshot::Receiver<String> {
        let (response_tx, response_rx) = oneshot::channel();
        let request_id = request.id.clone();
        let mut pending = self.pending_requests.lock().await;
        pending.insert(
            request_id,
            PendingRequest {
                request,
                response_tx,
            },
        );
        response_rx
    }

    /// 发送响应给等待中的请求
    pub async fn send_response(&self, request_id: &str, response: String) -> Result<()> {
        let mut pending = self.pending_requests.lock().await;
        if let Some(req) = pending.remove(request_id) {
            let _ = req.response_tx.send(response);
            Ok(())
        } else {
            anyhow::bail!("没有找到请求 ID: {}", request_id)
        }
    }

    /// 取消指定请求的挂起状态（客户端断开时调用）
    pub async fn cancel_pending(&self, request_id: &str) {
        let mut pending = self.pending_requests.lock().await;
        if pending.remove(request_id).is_some() {
            log_important!(info, "已取消挂起请求（客户端断开）: {}", request_id);
            // 通知上层清理 popup 窗口
            let _ = self.disconnect_tx.send(request_id.to_string()).await;
        }
    }

    /// 获取请求发送通道
    pub fn get_request_tx(&self) -> mpsc::Sender<IpcRequest> {
        self.request_tx.clone()
    }
}

/// IPC 服务器
pub struct IpcServer {
    state: Arc<IpcServerState>,
}

impl IpcServer {
    /// 创建新的 IPC 服务器
    pub fn new(request_tx: mpsc::Sender<IpcRequest>, disconnect_tx: mpsc::Sender<String>) -> Self {
        Self {
            state: Arc::new(IpcServerState::new(request_tx, disconnect_tx)),
        }
    }

    /// 获取状态引用
    pub fn state(&self) -> Arc<IpcServerState> {
        Arc::clone(&self.state)
    }

    /// 启动 IPC 服务器
    #[cfg(unix)]
    pub async fn start(&self) -> Result<()> {
        use tokio::net::UnixListener;

        let socket_path = get_socket_path();

        // 如果 socket 文件已存在，先删除
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)?;
        }

        // 创建 Unix socket 监听器
        let listener = UnixListener::bind(&socket_path)?;
        log_important!(info, "IPC 服务器已启动: {:?}", socket_path);

        let state = Arc::clone(&self.state);

        // 在后台任务中处理连接
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let state_clone = Arc::clone(&state);
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, state_clone).await {
                                log_important!(error, "处理 IPC 连接失败: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        log_important!(error, "接受 IPC 连接失败: {}", e);
                    }
                }
            }
        });

        Ok(())
    }

    #[cfg(windows)]
    pub async fn start(&self) -> Result<()> {
        log_important!(warn, "Windows IPC 服务器暂未实现");
        Ok(())
    }
}

/// 处理单个 IPC 连接
#[cfg(unix)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    state: Arc<IpcServerState>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    // 读取请求
    let bytes_read = buf_reader.read_line(&mut line).await?;
    if bytes_read == 0 {
        return Ok(());
    }

    // 解析请求
    let request: IpcRequest = serde_json::from_str(line.trim())?;
    let request_id = request.id.clone();

    log_important!(info, "收到 IPC 请求: {}，设置挂起请求", request_id);

    // 直接设置挂起请求（支持多请求并发）
    let response_rx = state.set_pending(request.clone()).await;

    log_important!(info, "请求 {} 已设置挂起，通知前端", request_id);

    // 通知前端有新请求
    let _ = state.get_request_tx().send(request).await;

    // 同时监听：用户响应 OR 客户端断开（MCP 进程被客户端重启）
    let mut disconnect_buf = [0u8; 1];
    tokio::select! {
        result = response_rx => {
            // 正常路径：用户通过前端提交了响应
            match result {
                Ok(response) => {
                    let ipc_response = IpcResponse {
                        id: request_id,
                        response,
                        success: true,
                        error: None,
                    };
                    let response_json = serde_json::to_string(&ipc_response)?;
                    writer.write_all(response_json.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                }
                Err(_) => {
                    let ipc_response = IpcResponse {
                        id: request_id,
                        response: String::new(),
                        success: false,
                        error: Some("响应通道已关闭".to_string()),
                    };
                    let response_json = serde_json::to_string(&ipc_response)?;
                    writer.write_all(response_json.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                }
            }
        }
        result = buf_reader.read(&mut disconnect_buf) => {
            // 客户端断开：MCP 进程被杀掉或重启
            match result {
                Ok(0) | Err(_) => {
                    log_important!(warn, "IPC 客户端已断开（请求 {}），释放挂起槽位", request_id);
                    state.cancel_pending(&request_id).await;
                }
                Ok(_) => {
                    // 客户端发送了额外数据（不应该发生），忽略
                }
            }
        }
    }

    Ok(())
}

/// 清理 socket 文件
pub fn cleanup_socket() {
    let socket_path = get_socket_path();
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
}
