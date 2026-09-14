import react from '@vitejs/plugin-react';
import { fileURLToPath, URL } from 'node:url';
import { defineConfig } from 'vite';

/**
 * 双入口构建配置（T-01，`02 §3` 目录结构）。
 *
 * - `pet`     → `index.html`    ：宠物窗口（透明、无边框、无任务栏项）
 * - `settings`→ `settings.html`：设置窗口（React 挂载点）
 *
 * 产物统一输出到 `src-tauri/dist`，与 `src-tauri/tauri.conf.json` 的
 * `build.frontendDist = "dist"` 对齐（Tauri 以 `src-tauri/` 为基准解析 ⇒ `src-tauri/dist`）。
 */
export default defineConfig({
  plugins: [react()],

  // 开发服务器：供 `tauri dev` 的 devUrl 使用，端口固定便于 tauri.conf.json 对齐。
  server: {
    port: 5173,
    strictPort: true,
    host: false,
  },

  build: {
    // Tauri 前端产物目录（相对本配置文件所在目录，即工程根）
    outDir: 'src-tauri/dist',
    emptyOutDir: true,
    sourcemap: process.env.NODE_ENV === 'development',
    rollupOptions: {
      input: {
        pet: fileURLToPath(new URL('./index.html', import.meta.url)),
        settings: fileURLToPath(new URL('./settings.html', import.meta.url)),
      },
    },
  },

  // Tauri 在 Windows 上以文件协议加载前端产物，禁用路径大小写敏感校验。
  resolve: {
    alias: {
      '@shared': fileURLToPath(new URL('./src/shared', import.meta.url)),
    },
  },

  clearScreen: false,
});
