/**
 * S8-M3：活动挂件纯逻辑单测（`formatRemaining` / `postcardText`）。
 * DOM 类（ActivityCard / PostcardWidget）与 DomLayers.ts 同处置：装配期构造，不进单测。
 */
import { describe, expect, it } from 'vitest';
import { formatRemaining, postcardText, POSTCARD_DWELL_MS } from './activityWidgets';

describe('formatRemaining', () => {
  it('≤0 → 即将回来', () => {
    expect(formatRemaining(0)).toBe('即将回来');
    expect(formatRemaining(-100)).toBe('即将回来');
    expect(formatRemaining(Number.NaN)).toBe('即将回来');
  });

  it('分钟档', () => {
    expect(formatRemaining(30 * 60_000)).toBe('30 分钟');
    expect(formatRemaining(30 * 60_000 + 500)).toBe('30 分钟');
    expect(formatRemaining(59 * 60_000 + 59_999)).toBe('59 分钟');
  });

  it('小时档', () => {
    expect(formatRemaining(60 * 60_000)).toBe('1 小时');
    expect(formatRemaining(90 * 60_000)).toBe('1 小时 30 分');
    expect(formatRemaining(120 * 60_000 + 1000)).toBe('2 小时');
  });
});

describe('postcardText', () => {
  it('TR-01 海边明信片带序号', () => {
    expect(postcardText('TR-01', 1)).toContain('第 1 张');
    expect(postcardText('TR-01', 3)).toContain('第 3 张');
    expect(postcardText('TR-01', 0)).toContain('第 1 张'); // 防御性兜底
  });

  it('非 TR-01 通用文案', () => {
    expect(postcardText('TR-02', 2)).toContain('第 2 张');
  });
});

describe('postcard 驻留时长', () => {
  it('D-2 驻留 20s（可手动关闭）', () => {
    expect(POSTCARD_DWELL_MS).toBe(20_000);
  });
});
