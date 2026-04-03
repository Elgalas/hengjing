<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import AppContent from './components/AppContent.vue'
import { useAppManager } from './composables/useAppManager'
import { usePopupAppManager } from './composables/usePopupAppManager'
import { useEventHandlers } from './composables/useEventHandlers'

// 检测当前窗口角色：popup-xxx 窗口使用独立的 PopupAppManager
const isPopupWindow = ref(false)
const windowLabel = getCurrentWebviewWindow().label
isPopupWindow.value = windowLabel.startsWith('popup-')

// 根据窗口角色选择不同的应用管理器
const manager = isPopupWindow.value ? usePopupAppManager() : useAppManager()

const {
  naiveTheme,
  mcpRequest,
  showMcpPopup,
  appConfig,
  isInitializing,
  actions,
} = manager

// 创建事件处理器
const handlers = useEventHandlers(actions)

// 主题应用由useTheme统一管理，移除重复的主题应用逻辑

// 初始化
onMounted(async () => {
  try {
    await actions.app.initialize()
  }
  catch (error) {
    console.error('应用初始化失败:', error)
  }
})

// 清理
onUnmounted(() => {
  actions.app.cleanup()
})
</script>

<template>
  <div class="min-h-screen bg-surface transition-colors duration-200">
    <n-config-provider :theme="naiveTheme">
      <n-message-provider>
        <n-notification-provider>
          <n-dialog-provider>
            <AppContent
              :mcp-request="mcpRequest" :show-mcp-popup="showMcpPopup" :app-config="appConfig"
              :is-initializing="isInitializing" @mcp-response="handlers.onMcpResponse" @mcp-cancel="handlers.onMcpCancel"
              @theme-change="handlers.onThemeChange" @toggle-always-on-top="handlers.onToggleAlwaysOnTop"
              @toggle-audio-notification="handlers.onToggleAudioNotification"
              @update-audio-url="handlers.onUpdateAudioUrl" @test-audio="handlers.onTestAudio"
              @stop-audio="handlers.onStopAudio" @test-audio-error="handlers.onTestAudioError"
              @update-window-size="handlers.onUpdateWindowSize"
              @update-reply-config="handlers.onUpdateReplyConfig" @message-ready="handlers.onMessageReady"
              @config-reloaded="handlers.onConfigReloaded"
            />
          </n-dialog-provider>
        </n-notification-provider>
      </n-message-provider>
    </n-config-provider>
  </div>
</template>
