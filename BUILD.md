# 恒境 - 编译和部署说明

## 环境要求

### 必需工具

| 工具 | 最低版本 | 用途 |
|------|---------|------|
| Rust (rustc + cargo) | 1.77+ | 后端编译 |
| Node.js | 18+ | 前端构建 |
| pnpm | 10+ | 前端包管理 |
| tauri-cli | 2.0+ | Tauri 打包（仅打包时需要） |

安装 tauri-cli：
```bash
cargo install tauri-cli
```

### 平台特定依赖

**macOS**:
- Xcode Command Line Tools (`xcode-select --install`)
- Tauri 依赖会通过 Cargo 自动解析

**Linux** (Ubuntu/Debian):
```bash
sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libssl-dev libasound2-dev
```

**Windows**:
- Visual Studio Build Tools (C++ 桌面开发工作负载)
- WebView2 Runtime (Windows 10/11 通常已内置)

## 快速编译

### 一键安装 (macOS/Linux)

```bash
./install.sh
```

该脚本执行以下操作：
1. 检查 `cargo` 和 `pnpm` 是否可用
2. `pnpm build` 构建前端资源到 `dist/`
3. `cargo build --release` 编译 Rust 后端
4. 将 `等`（UI 进程）和 `恒境`（MCP 服务器）复制到 `~/.local/bin/`

### 手动编译

```bash
# 1. 安装前端依赖
pnpm install

# 2. 构建前端
pnpm build

# 3. 编译 Rust（开发模式）
cargo build

# 4. 编译 Rust（发布模式）
cargo build --release
```

编译产物位于 `target/release/` 下：
- `等` — UI 进程（Tauri 应用，弹窗界面）
- `恒境` — MCP 服务器（CLI 进程）

### Tauri 打包（生成安装包）

```bash
pnpm tauri:build
```

产物位于 `target/release/bundle/`：
- macOS: `.dmg`, `.app`
- Linux: `.deb`, `.AppImage`
- Windows: `.msi`, `.exe`

## 部署

### CLI 模式部署（推荐）

将 `等` 和 `恒境` 放在同一目录或 `$PATH` 可达的位置：

```bash
# 方式1: 使用安装脚本
./install.sh   # 安装到 ~/.local/bin/

# 方式2: 手动复制
cp target/release/等 target/release/恒境 ~/.local/bin/

# 确保 PATH 包含目标目录
export PATH="$PATH:$HOME/.local/bin"
```

### MCP 客户端配置

在 MCP 客户端（如 Claude Desktop）的配置文件中添加：

```json
{
  "mcpServers": {
    "恒境": {
      "command": "恒境"
    }
  }
}
```

如果未加入 PATH，使用绝对路径：

```json
{
  "mcpServers": {
    "恒境": {
      "command": "/Users/<用户名>/.local/bin/恒境"
    }
  }
}
```

## 架构说明（多弹窗窗口）

### 运行模式

项目支持两种弹窗模式，运行时自动选择：

**IPC 模式（优先，macOS/Linux）**：
```
MCP 服务器(恒境) → Unix Socket → UI 进程(等) → 创建独立 popup 窗口
```
- UI 进程启动时创建 Unix Domain Socket 监听器（`$TMPDIR/hengjing-ui.sock`）
- MCP 请求通过 socket 发送到已运行的 UI 进程
- 每个请求创建独立的 popup 窗口（label: `popup-{requestId}`）
- 支持多请求并发处理

**子进程模式（回退）**：
```
MCP 服务器(恒境) → 启动 等 子进程 → 单次请求 → 进程退出
```
- 当 UI 进程未运行时自动回退
- Windows 平台当前始终使用此模式

### 关键模块

| 模块 | 路径 | 职责 |
|------|------|------|
| IPC 服务器 | `src/rust/ipc/server.rs` | Unix Socket 监听，请求-响应通道管理 |
| IPC 客户端 | `src/rust/ipc/client.rs` | 检测 UI 运行状态，发送请求 |
| IPC 命令 | `src/rust/ipc/commands.rs` | Tauri 命令，服务器启动与生命周期管理 |
| Popup 窗口 | `src/rust/ui/popup_windows.rs` | 窗口创建、会话状态、关闭/提交竞态控制 |
| MCP Handler | `src/rust/mcp/handlers/popup.rs` | 模式选择（IPC 优先 → 子进程回退） |
| 前端路由 | `src/frontend/App.vue` | 根据窗口 label 前缀选择 manager |
| Popup 会话 | `src/frontend/composables/usePopupSession.ts` | 请求加载、响应/取消处理 |

### 数据流

```
┌─────────────┐     Unix Socket     ┌──────────┐
│ MCP 服务器   │ ──────────────────→ │ UI 进程   │
│  (恒境)     │                     │  (等)     │
└─────────────┘                     └────┬─────┘
                                         │ 创建窗口
                                    ┌────▼─────┐
                                    │ popup-xxx │ ← 独立 WebviewWindow
                                    │  窗口     │
                                    └────┬─────┘
                                         │ 用户交互
                                    ┌────▼─────┐
                                    │ 响应回传   │ → oneshot channel → socket 写回
                                    └──────────┘
```

## 开发调试

### 开发模式

```bash
pnpm tauri:dev
```

前端热重载 + Rust 增量编译。

### 仅检查编译

```bash
# Rust 编译检查（不生成二进制）
cargo check

# 前端构建
pnpm build

# ESLint 检查
pnpm lint
```

### 测试弹窗

应用启动后，可通过开发者控制台调用测试命令：

```javascript
// 在主窗口的开发者工具中
await window.__TAURI__.core.invoke('create_test_popup', {
  request: JSON.stringify({
    id: "test-001",
    message: "测试弹窗消息",
    predefined_options: ["选项1", "选项2"],
    is_markdown: true
  })
})
```

## 平台支持状态

| 平台 | IPC 多弹窗 | 子进程回退 | 打包 |
|------|-----------|-----------|------|
| macOS | 支持 | 支持 | .dmg, .app |
| Linux | 支持 | 支持 | .deb, .AppImage |
| Windows | 暂不支持 | 支持 | .msi, .exe |

Windows 平台的 IPC 支持（Named Pipe）计划在后续版本实现。

## 常见问题

### socket 文件残留

如果 UI 进程异常退出，可能残留 socket 文件。IPC 客户端会自动检测并清理：
```bash
# 手动清理
rm $TMPDIR/hengjing-ui.sock
```

### 多实例冲突

同一时间只能运行一个 UI 进程实例。新启动的实例会清理旧的 socket 文件并接管监听。

### 编译错误排查

```bash
# 清理缓存重新编译
cargo clean && cargo build --release

# 检查 Tauri 特性编译
cargo check --features tauri/custom-protocol
```

### DMG 打包失败

macOS 上 `pnpm tauri:build` 可能在 DMG 生成阶段失败（签名/工具链问题），但 `.app` 已成功生成：
```bash
# 直接使用 .app
open target/release/bundle/macos/恒境.app

# 或手动安装二进制
cp target/release/等 target/release/恒境 ~/.local/bin/
```

### cargo build --release 后弹窗白屏

直接 `cargo build --release` 编译的二进制使用 `custom-protocol` 加载内嵌前端资源。
如果前端未通过 `pnpm build` 构建到 `dist/`，弹窗会白屏。

解决方法：使用 `pnpm tauri:build`（自动执行 `pnpm build` + `cargo build --release`），
或开发时使用 `pnpm tauri:dev`。
