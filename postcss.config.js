/**
 * PostCSS 配置（`package.json` 声明 `"type": "module"`，故使用 ESM 导出）。
 * Tailwind 负责样式生成，autoprefixer 负责浏览器前缀补全。
 */
export default {
  plugins: {
    tailwindcss: {},
    autoprefixer: {},
  },
};
