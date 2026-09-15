import React from 'react';

import { useTranslator } from '../store/useSettings';

/**
 * 成就 Tab（`01 §8.5` 成就页；本卡交付**骨架**）。
 *
 * 边界：成就定义与进度（≥25 个成就、达成撒花、`{name}` 模板重写）归 **S8-M5 养成与经济**
 * 与 S10-M1 增量面板。本卡只落地 Tab 结构、进度条与 i18n 键，**不伪造成就数据**
 * （`03 S5-M3`「禁止顺手改动：不新增未定义设置项」的同类纪律：不发明数据源）。
 *
 * 成就名按 `{name}` 模板重写（`01 FR-9-1` 例：「心心的第一个比心」）——本骨架里的
 * 文案同样只含 `{name}` 占位，等 S8-M5 提供定义后由内核渲染（C2 单一渲染点）。
 */
export function AchievementPage(): React.ReactElement {
  const t = useTranslator();

  return (
    <section className="dp-page" aria-label={t('app.tab.achievement')}>
      <h3 className="dp-section-title">{t('achievement.section')}</h3>
      <p className="dp-achievement-progress" data-testid="achievement-progress">
        {t('achievement.progress', { done: '0', total: '0' })}
      </p>
      <ul className="dp-achievement-list">
        <li className="dp-achievement-item dp-locked">
          <span className="dp-achievement-name">{t('achievement.locked')}</span>
        </li>
      </ul>
      <p className="dp-row-hint">{t('achievement.placeholder')}</p>
    </section>
  );
}

export default AchievementPage;
