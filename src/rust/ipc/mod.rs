//! IPC 模块
//!
//! 用于在已运行的 UI 进程（等）和 MCP 服务器/新进程之间传递请求与响应。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod client;
pub mod commands;
pub mod server;

pub use client::IpcClient;
pub use commands::{send_ipc_response, start_ipc_server, IpcStateWrapper};
pub use server::{cleanup_socket, IpcServer, IpcServerState};

/// IPC 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcRequest {
    pub id: String,
    pub message: String,
    pub predefined_options: Option<Vec<String>>,
    pub is_markdown: bool,
}

/// IPC 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub id: String,
    pub response: String,
    pub success: bool,
    pub error: Option<String>,
}

impl From<&crate::mcp::types::PopupRequest> for IpcRequest {
    fn from(request: &crate::mcp::types::PopupRequest) -> Self {
        Self {
            id: request.id.clone(),
            message: request.message.clone(),
            predefined_options: request.predefined_options.clone(),
            is_markdown: request.is_markdown,
        }
    }
}

impl From<IpcRequest> for crate::mcp::types::PopupRequest {
    fn from(request: IpcRequest) -> Self {
        Self {
            id: request.id,
            message: request.message,
            predefined_options: request.predefined_options,
            is_markdown: request.is_markdown,
        }
    }
}

/// 获取 IPC socket 路径
#[cfg(unix)]
pub fn get_socket_path() -> PathBuf {
    std::env::temp_dir().join("hengjing-ui.sock")
}

/// Windows 暂未实现（占位，避免跨平台编译错误）
#[cfg(windows)]
pub fn get_socket_path() -> PathBuf {
    std::env::temp_dir().join("hengjing-ui.sock")
}
