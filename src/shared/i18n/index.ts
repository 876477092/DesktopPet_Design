/**
 * i18n 骨架（`01 FR-7-7` / `02 §3` `shared/i18n`；`03 S5-M5`）。
 *
 * ## 设计口径
 * - **零依赖**：不引入 i18next 等运行时（C9 零网络 + 体积预算）；本骨架只有
 *   键值查表 + `{placeholder}` 替换两件事，够 `FR-7-7`「简中默认，预留 en-US」用；
 * - **flat dotted key**：键名 `域.分组.项`，查表 O(1)，且键集合可由单测机械比对
 *   （parity：`en-US` 必须与 `zh-CN` 同键集）；
 * - **回退链**：`当前语言 → zh-CN → 键名本身`。回退到键名是**故意可见的降级**：
 *   漏配文案时界面会显示 `appearance.scale.label` 而不是空白（便于 QA 一眼发现），
 *   与 `render_placeholders`「未命中 token 原样保留」同口径；
 * - **占位符口径与 Rust / 气泡层一致（C2）**：角色名一律 `{name}`，替换在渲染期完成，
 *   未命中的 token **原样保留**、不递归展开（与 `dp-core::emotion::lines::render_placeholders`
 *   和 `src/renderer/bubbleLogic.ts::renderPlaceholders` 三处同口径）。
 */

import { EN_US } from './en-US';
import { ZH_CN } from './zh-CN';

/** 支持的语言标识（BCP-47 子集；与 `settings.json.appearance.language` 同取值域）。 */
export type Locale = 'zh-CN' | 'en-US';

/** 全部受支持语言（顺序即设置页下拉顺序；首项为默认语言）。 */
export const LOCALES: readonly Locale[] = ['zh-CN', 'en-US'];

/** 默认语言（`01 FR-7-7`：简中默认）。 */
export const DEFAULT_LOCALE: Locale = 'zh-CN';

/** 文案表类型（flat dotted key → 文案）。 */
export type Messages = Record<string, string>;

/** 语言包目录（键集合由 [`missingKeys`] 机械校验）。 */
export const CATALOG: Record<Locale, Messages> = {
  'zh-CN': ZH_CN,
  'en-US': EN_US,
};

/**
 * 判定任意值是否为受支持语言（设置页下拉 / 存档字段的守门函数）。
 *
 * 存档里的 `appearance.language` 是自由字符串（`SettingsSave`），故消费前必须经此判定，
 * 非法值回退 [`DEFAULT_LOCALE`]（R19：脏配置不崩）。
 */
export function isLocale(value: unknown): value is Locale {
  return typeof value === 'string' && (LOCALES as readonly string[]).includes(value);
}

/** 归一化语言值：非法 / 缺失 → 默认语言。 */
export function resolveLocale(value: unknown): Locale {
  return isLocale(value) ? value : DEFAULT_LOCALE;
}

/**
 * `{token}` 占位符替换（与 Rust / 气泡层同口径，C2）。
 *
 * - 已命中的 token → 变量值；
 * - **未命中的 token 原样保留**（不删空、不递归展开）——让「漏传变量」在界面上可见；
 * - `vars` 中值为 `undefined` 视为未命中（不写入 `"undefined"`）。
 *
 * @param text 含 `{token}` 的文案
 * @param vars 变量表（如 `{ name: '心月狐' }`）
 */
export function renderPlaceholders(text: string, vars: Record<string, string>): string {
  return text.replace(/\{(\w+)\}/g, (match, token: string) => {
    const value = vars[token];
    return typeof value === 'string' && value.length > 0 ? value : match;
  });
}

/** 翻译函数签名。 */
export type Translator = (key: string, vars?: Record<string, string>) => string;

/**
 * 生成某语言的翻译函数（回退链：`locale → zh-CN → 键名`）。
 *
 * @param locale 目标语言（非法值经 [`resolveLocale`] 归一化）
 */
export function createTranslator(locale: unknown): Translator {
  const active = resolveLocale(locale);
  return (key: string, vars?: Record<string, string>): string => {
    const raw = CATALOG[active][key] ?? CATALOG[DEFAULT_LOCALE][key] ?? key;
    return vars === undefined ? raw : renderPlaceholders(raw, vars);
  };
}

/**
 * 列出某语言缺少的键（parity 单测用；运行时无需调用）。
 *
 * @param locale 待检查语言
 * @returns 缺少的键名列表（`[]` 表示与 zh-CN 完全对齐）
 */
export function missingKeys(locale: Locale): string[] {
  const reference = Object.keys(CATALOG[DEFAULT_LOCALE]);
  const target = CATALOG[locale];
  return reference.filter((key) => !(key in target));
}

/**
 * 列出某语言**多出**的键（拼写错误 / 废弃键会在此暴露）。
 *
 * @param locale 待检查语言
 * @returns 多余的键名列表（`[]` 表示无冗余）
 */
export function extraKeys(locale: Locale): string[] {
  const reference = new Set(Object.keys(CATALOG[DEFAULT_LOCALE]));
  return Object.keys(CATALOG[locale]).filter((key) => !reference.has(key));
}
