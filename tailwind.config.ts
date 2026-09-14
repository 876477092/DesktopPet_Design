import type { Config } from 'tailwindcss';

/**
 * Tailwind CSS 3 配置（T-01 骨架）。
 *
 * 双入口页面（宠物窗口 / 设置窗口）与全部 `src` 源码均为扫描范围。
 * 主题色板与组件类将在设置窗口各页面落地时（S1-M3 起）逐步扩充。
 */
export default {
  content: ['./index.html', './settings.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {},
  },
  plugins: [],
} satisfies Config;
