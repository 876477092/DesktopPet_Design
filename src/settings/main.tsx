import { createRoot } from 'react-dom/client';
import React from 'react';
import '../styles/settings.css';

/**
 * 设置窗口引导（T-01 骨架）。
 *
 * 本阶段只验证「React 能在 Tauri WebView2 中挂载」，不实现任何业务页面。
 * S1-M3 起按 `02 §3` 拆分出 `src/settings/App.tsx` 与 `pages/*`。
 */
function App(): React.ReactElement {
  return (
    <div className="flex h-full w-full flex-col items-center justify-center gap-3 bg-neutral-900 text-neutral-100">
      <h1 className="text-lg font-medium">desktop-pet · 设置</h1>
      <p className="text-sm text-neutral-400">工程骨架（T-01）：设置窗口已就绪。</p>
      <p className="text-sm text-neutral-400">
        角色名一律经 <code className="text-neutral-200">{'{name}'}</code> 占位符渲染（C2）。
      </p>
    </div>
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
      <App />
    </React.StrictMode>,
  );
}

bootstrapSettingsWindow();
