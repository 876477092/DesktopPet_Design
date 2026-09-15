import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import { SETTINGS_FALLBACK, type SettingsPatchV1, type SettingsSnapshotV1 } from '@shared/ipc';

import App, { TABS } from './App';
import BehaviorPage from './pages/BehaviorPage';
import DataPage, { formatBytes, saveStateKey } from './pages/DataPage';
import {
  SettingsContext,
  SettingsStore,
  applyPatchLocally,
  mergePatches,
  type SettingsPort,
} from './store/useSettings';
import { formatCombo, isModifierKey, normalizeKey } from './components/KeyCapture';

/**
 * 设置面板测试（`03 S5-M3` 验收：Tab 可切换 / 控件可用 / 改一项 UI 立即可见）。
 *
 * 覆盖策略（零新增测试依赖）：不用 DOM 测试库——用 `react-dom/server` 的
 * **静态渲染**断结构（Tab 集合、控件上下界来自快照、只读态提示），
 * 用 [`SettingsStore`] 的**纯逻辑**断言交互语义（即时可见 / 防抖合并 / 失败保留）。
 * 交互事件（点击、拖动）由「状态机 + 端口」这一层覆盖，不重复断言 DOM 事件。
 */

/** 记录调用的假端口（可断言提交内容与次数）。 */
function fakePort(overrides: Partial<SettingsPort> = {}): SettingsPort & {
  applied: SettingsPatchV1[];
  loadCount: number;
} {
  const applied: SettingsPatchV1[] = [];
  const port = {
    applied,
    loadCount: 0,
    load(): Promise<SettingsSnapshotV1> {
      port.loadCount += 1;
      return Promise.resolve({ ...SETTINGS_FALLBACK, writable: true });
    },
    apply(patch: SettingsPatchV1): Promise<SettingsSnapshotV1> {
      applied.push(patch);
      return Promise.resolve({ ...SETTINGS_FALLBACK, writable: true, revision: applied.length });
    },
    resetAll: () => Promise.resolve(),
    saveStatus: () => Promise.resolve(null),
    importSave: () => Promise.resolve(),
    emotionCommand: () => Promise.resolve(),
    subscribeConfig: () => Promise.resolve(() => undefined),
    ...overrides,
  };
  return port as SettingsPort & { applied: SettingsPatchV1[]; loadCount: number };
}

/** 用已加载的 store 静态渲染一个子树。 */
async function renderWithStore(node: React.ReactElement, store: SettingsStore): Promise<string> {
  await store.load();
  return renderToStaticMarkup(
    <SettingsContext.Provider value={store}>{node}</SettingsContext.Provider>,
  );
}

describe('设置面板 · 状态机', () => {
  it('本地快照在 patch 后立即更新（UI 即时可见），防抖后提交一次且合并多次改动', async () => {
    const port = fakePort();
    const store = new SettingsStore(port, { debounceMs: 5 });
    await store.load();

    store.patch({ scalePercent: 150 });
    // 尚未提交，但本地已可见（`01 FR-7-4` 每项即时生效）。
    expect(store.getState().snapshot.scalePercent).toBe(150);
    expect(store.getState().dirty).toBe(true);
    expect(port.applied).toHaveLength(0);

    store.patch({ opacityPercent: 80 });
    expect(store.getState().snapshot.opacityPercent).toBe(80);

    await new Promise((resolve) => setTimeout(resolve, 20));
    // 两次改动合并为**一次**提交（拖动滑块不会打出 N 次 IPC）。
    expect(port.applied).toHaveLength(1);
    expect(port.applied[0]).toEqual({ scalePercent: 150, opacityPercent: 80 });
    expect(store.getState().dirty).toBe(false);
    expect(store.getState().snapshot.writable).toBe(true);
    store.dispose();
  });

  it('「保存」按钮 = 立即提交待发补丁（不是第二条写入路径）', async () => {
    const port = fakePort();
    const store = new SettingsStore(port, { debounceMs: 10_000 });
    await store.load();

    store.patch({ muted: true });
    expect(port.applied).toHaveLength(0);
    await store.flush();
    expect(port.applied).toEqual([{ muted: true }]);
    // 无待提交补丁时 flush 是 no-op（不产生空提交）。
    await store.flush();
    expect(port.applied).toHaveLength(1);
    store.dispose();
  });

  it('提交失败保留待提交改动（不丢用户改动）并记录错误', async () => {
    const port = fakePort({
      apply: () => Promise.reject(new Error('内核不在线')),
    });
    const store = new SettingsStore(port, { debounceMs: 0 });
    await store.load();

    store.patch({ autoRoam: false });
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(store.getState().error).toBe('内核不在线');
    expect(store.getState().dirty).toBe(true);
    expect(store.getState().pending).toEqual({ autoRoam: false });
    // 重试成功后清空（同一条补丁不会被丢掉）。
    store.dispose();
  });

  it('只读态（writable=false）不提交，改动只停在本地预览', async () => {
    const port = fakePort({
      load: () => Promise.resolve({ ...SETTINGS_FALLBACK, writable: false }),
    });
    const store = new SettingsStore(port, { debounceMs: 0 });
    await store.load();
    expect(store.getState().writable).toBe(false);

    store.patch({ scalePercent: 120 });
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(port.applied).toHaveLength(0);
    expect(store.getState().snapshot.scalePercent).toBe(120);
    store.dispose();
  });

  it('载入失败降级为内置兜底 + 只读预览（不阻断窗口显示）', async () => {
    const port = fakePort({ load: () => Promise.reject(new Error('设置服务尚未装配')) });
    const store = new SettingsStore(port, { debounceMs: 0 });
    await store.load();
    expect(store.getState().loading).toBe(false);
    expect(store.getState().writable).toBe(false);
    expect(store.getState().snapshot).toEqual(SETTINGS_FALLBACK);
    expect(store.getState().error).toBe('设置服务尚未装配');
    store.dispose();
  });

  it('pet://config 的 revision 变化触发重取快照（相同 revision 不重取）', async () => {
    const port = fakePort();
    const store = new SettingsStore(port, { debounceMs: 0 });
    await store.load();
    expect(port.loadCount).toBe(1);

    store.onConfigEvent({ version: 1, revision: 0, changed: [], persisted: true });
    expect(port.loadCount).toBe(1);
    store.onConfigEvent({ version: 1, revision: 3, changed: ['audio'], persisted: true });
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(port.loadCount).toBe(2);
    store.dispose();
  });
});

describe('设置面板 · 补丁工具（纯函数）', () => {
  it('mergePatches 合并 reminders 子对象并按字段覆盖', () => {
    const merged = mergePatches(
      { reminders: { waterIntervalMin: 60 }, muted: true },
      { reminders: { sedentaryIntervalMin: 30 } },
    );
    expect(merged).toEqual({
      muted: true,
      reminders: { waterIntervalMin: 60, sedentaryIntervalMin: 30 },
    });
  });

  it('applyPatchLocally 按快照自带取值域夹紧（不发明数值）', () => {
    const next = applyPatchLocally(SETTINGS_FALLBACK, {
      scalePercent: 9999,
      opacityPercent: 1,
      masterVolumePercent: 500,
      reminders: { waterIntervalMin: 9999 },
    });
    expect(next.scalePercent).toBe(SETTINGS_FALLBACK.scaleMaxPercent);
    expect(next.opacityPercent).toBe(SETTINGS_FALLBACK.opacityMinPercent);
    expect(next.masterVolumePercent).toBe(SETTINGS_FALLBACK.volumeMaxPercent);
    expect(next.reminders.waterIntervalMin).toBe(SETTINGS_FALLBACK.reminders.intervalMaxMin);
  });
});

describe('设置面板 · 结构（静态渲染）', () => {
  it('外壳渲染 6 个 Tab，标题走 {name} 占位（C2）', async () => {
    const store = new SettingsStore(fakePort(), { debounceMs: 0 });
    const html = await renderWithStore(<App />, store);
    expect(TABS).toHaveLength(6);
    for (const tab of TABS) {
      expect(html).toContain(`data-testid="tab-${tab}"`);
    }
    // 名字为空时回退 defaultName（也是空 → 占位保留，不硬编码角色名）。
    expect(html).toContain('设置');
    expect(html).not.toContain(String.fromCodePoint(0x5fc3, 0x6708, 0x72d0));
    store.dispose();
  });

  it('默认 Page 为外观页，且滑块上下界来自快照而非字面量', async () => {
    const store = new SettingsStore(
      fakePort({
        load: () =>
          Promise.resolve({
            ...SETTINGS_FALLBACK,
            writable: true,
            scaleMinPercent: 20,
            scaleMaxPercent: 300,
            scaleStepPercent: 5,
            scalePercent: 120,
          }),
      }),
      { debounceMs: 0 },
    );
    const html = await renderWithStore(<App />, store);
    // 上下界取自快照（若 UI 硬编码 50/200，本用例会失败）。
    expect(html).toContain('id="appearance-scale"');
    expect(html).toContain('min="20"');
    expect(html).toContain('max="300"');
    expect(html).toContain('step="5"');
    store.dispose();
  });

  it('行为页音量滑块上下界与勿扰/提醒开关均来自快照', async () => {
    const store = new SettingsStore(
      fakePort({
        load: () =>
          Promise.resolve({
            ...SETTINGS_FALLBACK,
            writable: true,
            volumeMaxPercent: 70,
            doNotDisturb: true,
            reminders: { ...SETTINGS_FALLBACK.reminders, sedentaryIntervalMin: 30 },
          }),
      }),
      { debounceMs: 0 },
    );
    const html = await renderWithStore(<BehaviorPage />, store);
    expect(html).toContain('id="behavior-volume"');
    expect(html).toContain('max="70"');
    expect(html).toContain('id="behavior-dnd"');
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain('id="reminder-sedentary-interval"');
    store.dispose();
  });

  it('数据页在没有可用备份时给出明确提示（不出现空列表）', async () => {
    const store = new SettingsStore(fakePort(), { debounceMs: 0 });
    const html = await renderWithStore(<DataPage />, store);
    expect(html).toContain('data-testid="no-backup"');
    expect(html).toContain('data-testid="save-path"');
    store.dispose();
  });
});

describe('设置面板 · 存档展示工具（纯函数）', () => {
  it('saveStateKey 覆盖全部加载落点并保守兜底未知状态', () => {
    expect(saveStateKey('loaded')).toBe('data.save.state.loaded');
    expect(saveStateKey('isolatedCorrupt')).toBe('data.save.state.isolated');
    expect(saveStateKey('migrationPending')).toBe('data.save.state.migrationPending');
    expect(saveStateKey('somethingNew')).toBe('data.save.state.isolated');
  });

  it('formatBytes 输出可读体积', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(2048)).toBe('2.0 KB');
    expect(formatBytes(3 * 1024 * 1024)).toBe('3.0 MB');
  });
});

describe('设置面板 · 快捷键捕获（纯函数）', () => {
  it('组合键规范化：修饰键固定顺序 + 主键大写', () => {
    expect(
      formatCombo({ key: 'k', ctrlKey: true, altKey: false, shiftKey: true, metaKey: false }),
    ).toBe('Ctrl+Shift+K');
    expect(
      formatCombo({ key: ' ', ctrlKey: false, altKey: true, shiftKey: false, metaKey: false }),
    ).toBe('Alt+Space');
    expect(
      formatCombo({ key: 'F5', ctrlKey: false, altKey: false, shiftKey: false, metaKey: false }),
    ).toBe('F5');
  });

  it('纯修饰键不产生组合（避免捕获半截组合）', () => {
    for (const key of ['Control', 'Alt', 'Shift', 'Meta']) {
      expect(isModifierKey(key)).toBe(true);
      expect(formatCombo({ key, ctrlKey: true, altKey: false, shiftKey: false, metaKey: false })).toBeNull();
    }
    expect(normalizeKey('Enter')).toBe('Enter');
  });
});
