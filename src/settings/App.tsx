import React, { useMemo, useState } from 'react';

import { resolveLocale } from '@shared/i18n';

import ConfirmDialog from './components/ConfirmDialog';
import AchievementPage from './pages/AchievementPage';
import AppearancePage from './pages/AppearancePage';
import BehaviorPage from './pages/BehaviorPage';
import DataPage from './pages/DataPage';
import InteractionPage from './pages/InteractionPage';
import NeedsPage from './pages/NeedsPage';
import { useSettingsState, useSettingsStore, useSettingsWritable, useTranslator } from './store/useSettings';

/**
 * 设置面板外壳（`01 §8.3` 480×640 悬浮窗；`03 S5-M3` 交付物 `App.tsx`）。
 *
 * ## Tab 集合裁定（本卡口径，与 `01 §8.3` 线框的差异已登记）
 * `01 §8.3` 的线框 Tab 条画的是 `[外观][行为][属性][活动][数据]`（5 项，且注明
 * 「『属性』『活动』Tab 见 §6.12.6 / §6.13.6」）；`03 S5-M3` 卡片把 Tab 冻结为
 * **外观 / 行为 / 互动 / 属性 / 数据 / 成就**（6 项），且其交付物清单同样列这 6 个
 * `*Page.tsx`。本卡按**卡片**执行（`03` 卡片是编码期唯一输入），差异原因：
 *   - 「活动」Tab 属 `FR-13` 外出活动，其数据源归 **S8-M1/M4**（活动状态机）；
 *   - 「商城 / 背包」Tab 属 `FR-14`，归 **S8-M6**；
 *   - 二者在 `02 §3` 的文件清单里也属后续模块（`ActivityPage` / `ShopPage`）。
 * 故本卡的 Tab 条**数据驱动**（见 [`TABS`]），S8/S10 追加 Tab 只需在数组里加一项。
 *
 * ## 生效时序
 * 每项改动即调 `store.patch()`：本地快照立即更新（UI 即时可见）+ 防抖提交给内核；
 * 底部「保存」= 立即提交待发补丁（`01 §8.3` 的按钮语义，不是第二条写入路径）。
 */
export const TABS = [
  'appearance',
  'behavior',
  'interaction',
  'needs',
  'data',
  'achievement',
] as const;

/** Tab 标识。 */
export type TabId = (typeof TABS)[number];

/** Tab 渲染（数据驱动：`TABS` 决定顺序，`PAGES` 决定内容）。 */
const PAGES: Record<TabId, () => React.ReactElement> = {
  appearance: AppearancePage,
  behavior: BehaviorPage,
  interaction: InteractionPage,
  needs: NeedsPage,
  data: DataPage,
  achievement: AchievementPage,
};

/** 设置面板外壳。 */
export function App(): React.ReactElement {
  const store = useSettingsStore();
  const { snapshot, dirty, saving, error, notice, loading } = useSettingsState();
  const t = useTranslator();
  const writable = useSettingsWritable();
  const [tab, setTab] = useState<TabId>('appearance');
  const [confirmingReset, setConfirmingReset] = useState(false);

  const Current = PAGES[tab];
  const title = useMemo(
    () => t('app.title', { name: snapshot.name || snapshot.defaultName }),
    [t, snapshot.name, snapshot.defaultName],
  );

  return (
    <div className="dp-app" data-testid="settings-app">
      <header className="dp-title">
        <h1 className="dp-title-text">{title}</h1>
        {!writable && <span className="dp-badge">{t('app.readonly.hint')}</span>}
      </header>

      <nav className="dp-tabs" role="tablist" aria-label={title}>
        {TABS.map((id) => (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={tab === id}
            className={`dp-tab${tab === id ? ' dp-tab-on' : ''}`}
            data-testid={`tab-${id}`}
            onClick={() => setTab(id)}
          >
            {t(`app.tab.${id}`)}
          </button>
        ))}
      </nav>

      <main className="dp-content" role="tabpanel">
        {loading ? <p className="dp-row-hint">{t('common.loading')}</p> : <Current />}
      </main>

      <footer className="dp-footer">
        <button
          type="button"
          className="dp-btn dp-btn-danger"
          disabled={!writable}
          onClick={() => setConfirmingReset(true)}
        >
          {t('app.footer.reset')}
        </button>
        <span className="dp-status" role="status">
          {error !== null ? error : notice !== null ? t(notice) : dirty ? t('app.dirty.hint') : ''}
        </span>
        <button
          type="button"
          className="dp-btn dp-btn-primary"
          disabled={!writable || saving || !dirty}
          onClick={() => void store.flush()}
        >
          {t('app.footer.save')}
        </button>
      </footer>

      <ConfirmDialog
        open={confirmingReset}
        title={t('data.reset.confirm.title')}
        body={t('data.reset.confirm.body')}
        confirmLabel={t('common.confirm')}
        cancelLabel={t('common.cancel')}
        onCancel={() => setConfirmingReset(false)}
        onConfirm={() => {
          setConfirmingReset(false);
          void store.resetAll();
        }}
      />
    </div>
  );
}

/** 当前语言（供 `main.tsx` 设置 `<html lang>` 等宿主属性用）。 */
export function currentLocale(language: string): string {
  return resolveLocale(language);
}

export default App;
