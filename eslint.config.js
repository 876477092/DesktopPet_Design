import js from '@eslint/js';
import tseslint from 'typescript-eslint';

/**
 * ESLint 9 flat config（T-01 骨架）。
 *
 * 约定：
 * - TypeScript 侧沿用 `typescript-eslint` recommended（已关闭与类型系统重复的 `no-undef` 等规则）。
 * - `scripts/*.mjs` 为纯 Node ESM，仅给定最小全局变量集。
 * - 忽略构建产物目录（dist / src-tauri / node_modules）。
 */
export default tseslint.config(
  {
    ignores: [
      'node_modules/**',
      'dist/**',
      'src-tauri/**',
      'assets/**',
      'resources/**',
      'coverage/**',
    ],
  },

  js.configs.recommended,
  ...tseslint.configs.recommended,

  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      parserOptions: {
        ecmaFeatures: { jsx: true },
      },
    },
    rules: {
      // 标识符遵循 `02 §7.1`；此处不做额外风格约束，交由 tsc strict 把关。
      '@typescript-eslint/consistent-type-imports': ['error', { prefer: 'type-imports' }],
    },
  },

  {
    files: ['**/*.mjs', '**/*.js'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: {
        console: 'readonly',
        process: 'readonly',
        URL: 'readonly',
        setTimeout: 'readonly',
        clearTimeout: 'readonly',
      },
    },
  },
);
