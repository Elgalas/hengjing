import type { McpRequest } from '../types/popup'
import { invoke } from '@tauri-apps/api/core'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import { ref } from 'vue'

/**
 * Popup 窗口会话管理
 *
 * 用于 popup-{requestId} 窗口，通过窗口 label 查询绑定的请求
 */
export function usePopupSession() {
  const mcpRequest = ref<McpRequest | null>(null)
  const showMcpPopup = ref(true)

  /**
   * 加载当前窗口绑定的请求
   */
  async function loadCurrentRequest() {
    try {
      const request = await invoke<McpRequest | null>('get_popup_request_for_current_window')
      if (!request) {
        throw new Error('当前 popup 未绑定请求')
      }
      mcpRequest.value = request
      return request
    }
    catch (error) {
      console.error('加载 popup 请求失败:', error)
      await getCurrentWebviewWindow().destroy()
      throw error
    }
  }

  /**
   * 发送 popup 响应（通过 request_id 路由到正确的 IPC 通道）
   */
  async function handleMcpResponse(response: any) {
    const currentRequest = mcpRequest.value
    if (!currentRequest?.id) {
      throw new Error('当前 popup 没有可响应的请求')
    }

    await invoke('send_popup_response', {
      requestId: currentRequest.id,
      response,
    })
    mcpRequest.value = null
  }

  /**
   * 取消 popup 请求
   */
  async function handleMcpCancel() {
    const currentRequest = mcpRequest.value
    if (!currentRequest?.id) {
      throw new Error('当前 popup 没有可取消的请求')
    }

    await invoke('cancel_popup_request', {
      requestId: currentRequest.id,
    })
    mcpRequest.value = null
  }

  return {
    mcpRequest,
    showMcpPopup,
    loadCurrentRequest,
    handleMcpResponse,
    handleMcpCancel,
  }
}
