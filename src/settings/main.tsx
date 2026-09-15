import { createRoot } from 'react-dom/client';
import React from 'react';

import '../styles/settings.css';
import App from './App';
import { SettingsContext, createIpcSettingsPort, createNullSettingsPort, useSettingsStoreLifecycle } from './store/useSettings';

/**
 * 设置窗口引导（`03 S5-M3` 交付物；替换 T-01 骨架）。
 *
 * 端口选择：运行在 Tauri WebView 内时用真实 IPC 端口（`settings_get` / `settings_apply` …）；
 * 其余环境（浏览器直开 `settings.html` 做视觉走查 / vitest）退化为只读端口——
 * 这样 UI 永远能挂载（`02 §7.4.2` 降级不崩），只是改不动设置。
 *
 * 判定方式：Tauri 2 在 WebView 内注入 `window.__TAURI_INTERNALS__`；本模块**只读该标记**
 * 而不导入 `@tauri-apps/api` 的运行时判定，避免在非 Tauri 环境下抛错。
 */
function hasTauriRuntime(): boolean {
  return typeof (globalThis as Record<string, unknown>).__TAURI_INTERNALS__ !== 'undefined';
}

/** 挂载设置面板（含 Provider 装配）。 */
function SettingsRoot(): React.ReactElement {
  const port = React.useMemo(
    () => (hasTauriRuntime() ? createIpcSettingsPort() : createNullSettingsPort()),
    [],
  );
  const store = useSettingsStoreLifecycle(port);
  return (
    <SettingsContext.Provider value={store}>
      <App />
    </SettingsContext.Provider>
  );
}

/** 挂载 React 根节点，容器缺失时显式报错而不是静默失败（`02 §7.4`）。 */
function bootstrapSettingsWindow(): void {
  const container = document.getElementById('settings-root');
  if (container === null) {
    throw new Error('未找到挂载点 #settings-root');
  }

  createRoot(container).render(
    <React.StrictMode>
      <SettingsRoot />
    </React.StrictMode>,
  );
}

bootstrapSettingsWindow();
