import React from 'react';

import { usePetSnapshot } from '../hooks/usePetSnapshot';
import { useTranslator } from '../store/useSettings';
import NeedsBar from '../components/NeedsBar';

/**
 * 属性 Tab（`01 §6.12.6` 六维属性面板；S10-M1 接入实时值）。
 *
 * 数据源：`pet://state` 的 `values` 段（core-loop 1Hz 推送）。未连接内核时
 * （`snap === null`）渲染空态占位，不伪造数值（`03 S5-M3` 纪律：不发明数据源）。
 *
 * 六维名称为 `01 §6.12.1` 冻结词表；亲密度为等级制（Lv1~10 + 级内经验），
 * 与其余五维的 0~100 条区分显示。
 */
export function NeedsPage(): React.ReactElement {
  const t = useTranslator();
  const snap = usePetSnapshot();
  const v = snap?.values;

  if (v === undefined) {
    return (
      <section className="dp-page" aria-label={t('app.tab.needs')}>
        <p className="dp-row-hint">{t('common.loading')}</p>
      </section>
    );
  }

  const affinityDisplay =
    v.affinityExpNext > 0
      ? t('needs.affinity.progress', {
          level: String(v.affinityLevel),
          exp: String(Math.round(v.affinityExp)),
          next: String(Math.round(v.affinityExpNext)),
        })
      : t('needs.affinity.max', { level: String(v.affinityLevel) });

  return (
    <section className="dp-page" aria-label={t('app.tab.needs')}>
      <ul className="dp-needs-list">
        <NeedsBar label={t('needs.mood')} value={v.mood} testId="mood" />
        <NeedsBar label={t('needs.energy')} value={v.energy} testId="energy" />
        <NeedsBar label={t('needs.satiety')} value={v.satiety} testId="satiety" />
        <NeedsBar label={t('needs.cleanliness')} value={v.cleanliness} testId="cleanliness" />
        <NeedsBar
          label={t('needs.affinity')}
          value={v.affinityExpNext > 0 ? (v.affinityExp / v.affinityExpNext) * 100 : 100}
          display={affinityDisplay}
          testId="affinity"
        />
        <NeedsBar label={t('needs.boredom')} value={v.boredom} testId="boredom" />
      </ul>
    </section>
  );
}

export default NeedsPage;
