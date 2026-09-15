import React from 'react';

import { useTranslator } from '../store/useSettings';

/**
 * 属性 Tab（`01 §6.12.6` 六维属性面板；本卡交付**骨架**）。
 *
 * 边界（`03 S5-M3`「单次会话边界：只做 UI 与控件」）：六维的实时数值由内核推送
 * （`pet://state` / `pet://needs`，消费者归 **S7-M1 需求系统**与 **S7-M4 自适应**），
 * 本卡不伪造数值、不自行计算——只把六维名称与占位说明渲染出来，保证 Tab 结构与
 * i18n 键已就位，S7 接入时只替换数据源。
 *
 * 六维**名称**（而非数值）在此列出是刻意的：它们属 `01 §6.12.1` 冻结词表，
 * 提前落地可让「属性面板」在 S7 前就能被视觉验收（不出现空白 Tab）。
 */
export function NeedsPage(): React.ReactElement {
  const t = useTranslator();

  const dimensions: ReadonlyArray<{ key: string; label: string }> = [
    { key: 'satiety', label: t('needs.satiety') },
    { key: 'cleanliness', label: t('needs.cleanliness') },
    { key: 'energy', label: t('needs.energy') },
    { key: 'mood', label: t('needs.mood') },
    { key: 'affinity', label: t('needs.affinity') },
    { key: 'boredom', label: t('needs.boredom') },
  ];

  return (
    <section className="dp-page" aria-label={t('app.tab.needs')}>
      <ul className="dp-needs-list">
        {dimensions.map((item) => (
          <li className="dp-needs-item" key={item.key} data-testid={`needs-${item.key}`}>
            <span className="dp-needs-name">{item.label}</span>
            <span className="dp-needs-value" aria-hidden="true">
              {t('common.unavailable')}
            </span>
            <div className="dp-needs-bar" role="presentation">
              <span className="dp-needs-bar-fill" style={{ width: '0%' }} />
            </div>
          </li>
        ))}
      </ul>
      <p className="dp-row-hint">{t('needs.placeholder')}</p>
    </section>
  );
}

export default NeedsPage;
