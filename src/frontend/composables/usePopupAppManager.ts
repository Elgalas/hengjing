import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { useAudioManager } from './useAudioManager'
import { useFontManager } from './useFontManager'
import { usePopupSession } from './usePopupSession'
import { useSettings } from './useSettings'
import { useTheme } from './useTheme'

/**
 * Popup 窗口专用的应用管理器
 *
 * 与 useAppManager 的区别：
 * - 使用 usePopupSession 替代 useMcpHandler（通过窗口 label 查询请求）
 * - 不初始化 MCP 工具配置、MCP 事件监听
 * - 不进行版本检查
 */
export function usePopupAppManager() {
  const theme = useTheme()
  const settings = useSettings()
  const audioManager = useAudioManager()
  const popupSession = usePopupSession()
  const { loadFontConfig, loadFontOptions } = useFontManager()
  const isInitializing = ref(true)

  const appConfig = computed(() => {
    return {
      theme: theme.currentTheme.value,
      window: {
        alwaysOnTop: settings.alwaysOnTop.value,
        width: settings.windowWidth.value,
        height: settings.windowHeight.value,
        fixed: settings.fixedWindowSize.value,
      },
      audio: {
        enabled: settings.audioNotificationEnabled.value,
        url: settings.audioUrl.value,
      },
      reply: {
        enabled: settings.continueReplyEnabled.value,
        prompt: settings.continuePrompt.value,
      },
    }
  })

  /**
   * 初始化 popup 窗口应用
   */
  async function initializePopupApp() {
    try {
      // 加载字体设置
      await Promise.all([
        loadFontConfig(),
        loadFontOptions(),
      ])

      // 加载窗口设置
      await settings.loadWindowSettings()
      await settings.loadWindowConfig()
      await settings.setupWindowFocusListener()

      // 同步窗口状态
      try {
        await settings.syncWindowStateFromBackend()
      }
      catch (error) {
        console.warn('popup 窗口状态同步失败，继续初始化:', error)
      }

      // 加载当前窗口绑定的请求
      const request = await popupSession.loadCurrentRequest()

      // 启动 Telegram 同步
      try {
        if (request?.message) {
          await invoke('start_telegram_sync', {
            message: request.message,
            predefinedOptions: request.predefined_options || [],
            isMarkdown: request.is_markdown || false,
            clientName: request.client_name || null,
            requestId: request.id || null,
          })
        }
      }
      catch (error) {
        console.error('启动Telegram同步失败:', error)
      }

      isInitializing.value = false
    }
    catch (error) {
      isInitializing.value = false
      throw error
    }
  }

  const actions = {
    theme: {
      setTheme: theme.setTheme,
    },
    settings: {
      toggleAlwaysOnTop: settings.toggleAlwaysOnTop,
      toggleAudioNotification: settings.toggleAudioNotification,
      updateAudioUrl: settings.updateAudioUrl,
      testAudio: settings.testAudioSound,
      stopAudio: settings.stopAudioSound,
      updateWindowSize: settings.updateWindowSize,
      updateReplyConfig: settings.updateReplyConfig,
      setMessageInstance: settings.setMessageInstance,
      reloadAllSettings: settings.reloadAllSettings,
    },
    mcp: {
      handleResponse: popupSession.handleMcpResponse,
      handleCancel: popupSession.handleMcpCancel,
    },
    audio: {
      handleTestError: audioManager.handleTestAudioError,
    },
    app: {
      initialize: initializePopupApp,
      cleanup: () => {
        settings.removeWindowFocusListener()
      },
    },
  }

  return {
    naiveTheme: theme.naiveTheme,
    mcpRequest: popupSession.mcpRequest,
    showMcpPopup: popupSession.showMcpPopup,
    appConfig,
    isInitializing,
    actions,
  }
}
