import { describe, expect, it } from 'vitest';

import {
  CATALOG,
  DEFAULT_LOCALE,
  LOCALES,
  createTranslator,
  extraKeys,
  isLocale,
  missingKeys,
  renderPlaceholders,
  resolveLocale,
} from './index';
import { ZH_CN } from './zh-CN';
import { EN_US } from './en-US';

/**
 * i18n 骨架单测（`03 S5-M5` 验收：切换语言 → UI 文案随之切换；默认中文）。
 *
 * 断言分四组：
 *   1. 默认语言与语言集合；
 *   2. **键集合 parity**（en-US 必须与 zh-CN 同键集——缺键 = 切语言后部分文案回退中文）；
 *   3. 回退链（`当前 → zh-CN → 键名`）；
 *   4. 占位符口径（与 Rust / 气泡层同口径，C2：未命中 token 原样保留）。
 */
describe('i18n 骨架', () => {
  it('默认语言为简体中文，且语言集合与 settings 取值域一致', () => {
    expect(DEFAULT_LOCALE).toBe('zh-CN');
    expect(LOCALES).toEqual(['zh-CN', 'en-US']);
    expect(Object.keys(CATALOG).sort()).toEqual([...LOCALES].sort());
  });

  it('en-US 与 zh-CN 键集合完全一致（无缺失、无冗余）', () => {
    expect(missingKeys('en-US')).toEqual([]);
    expect(extraKeys('en-US')).toEqual([]);
    // 自检：本测试真的在比对非空集合。
    expect(Object.keys(ZH_CN).length).toBeGreaterThan(60);
    expect(Object.keys(EN_US).length).toBe(Object.keys(ZH_CN).length);
  });

  it('语言守门与归一化：非法值回退默认中文', () => {
    expect(isLocale('zh-CN')).toBe(true);
    expect(isLocale('en-US')).toBe(true);
    expect(isLocale('ja-JP')).toBe(false);
    expect(isLocale(undefined)).toBe(false);
    expect(isLocale(42)).toBe(false);
    expect(resolveLocale('en-US')).toBe('en-US');
    expect(resolveLocale('ja-JP')).toBe('zh-CN');
    expect(resolveLocale(null)).toBe('zh-CN');
  });

  it('同一键在两种语言下返回各自文案（切换语言 → 文案随之切换）', () => {
    const zh = createTranslator('zh-CN');
    const en = createTranslator('en-US');
    expect(zh('app.tab.appearance')).toBe('外观');
    expect(en('app.tab.appearance')).toBe('Appearance');
    expect(zh('app.footer.reset')).toBe('重置全部数据');
    expect(en('app.footer.reset')).toBe('Reset all data');
    expect(zh('app.tab.appearance')).not.toBe(en('app.tab.appearance'));
  });

  it('缺键回退链：当前语言缺 → zh-CN → 键名本身（可见降级，不漏空白）', () => {
    const en = createTranslator('en-US');
    // 已存在键走自身语言。
    expect(en('common.ok')).toBe('OK');
    // 未知键 → 原样返回键名（便于 QA 一眼发现漏配）。
    expect(en('nope.not.exists')).toBe('nope.not.exists');
    expect(createTranslator('zh-CN')('nope.not.exists')).toBe('nope.not.exists');
    // 非法语言 → 归一化为中文（回退链仍在）。
    expect(createTranslator('de-DE')('common.ok')).toBe(ZH_CN['common.ok']);
  });

  it('占位符替换：已命中替换、未命中原样保留、不递归展开（与 Rust 同口径，C2）', () => {
    expect(renderPlaceholders('{name} 设置', { name: 'X' })).toBe('X 设置');
    // 未命中 token 原样保留（不删空、不写入 "undefined"）。
    expect(renderPlaceholders('{name} 设置', {})).toBe('{name} 设置');
    expect(renderPlaceholders('{a}{b}', { b: 'B' })).toBe('{a}B');
    // 空串视为未命中（避免渲染成空白角色名）。
    expect(renderPlaceholders('{name}', { name: '' })).toBe('{name}');
    // 不递归展开：值里再含 token 时原样输出。
    expect(renderPlaceholders('{a}', { a: '{b}', b: 'B' })).toBe('{b}');
  });

  it('翻译函数同样走占位符替换（含 {name} 的角色名注入）', () => {
    const zh = createTranslator('zh-CN');
    expect(zh('dialog.quit.title', { name: 'X' })).toBe('退出 X？');
    // 未传变量 → token 原样保留（绝不回退到硬编码角色名）。
    expect(zh('dialog.quit.title')).toBe('退出 {name}？');
  });

  it('文案里零角色名硬编码（C2：一律走 {name} 占位）', () => {
    const roleName = String.fromCodePoint(0x5fc3, 0x6708, 0x72d0);
    for (const [locale, messages] of Object.entries(CATALOG)) {
      for (const [key, text] of Object.entries(messages)) {
        expect(text.includes(roleName), `${locale}:${key} 不得硬编码角色名`).toBe(false);
      }
    }
  });
});
