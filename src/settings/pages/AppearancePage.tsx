import React from 'react';

import { LOCALES, resolveLocale, type Locale } from '@shared/i18n';

import Segmented from '../components/Segmented';
import Slider from '../components/Slider';
import { useSettingsState, useSettingsStore, useSettingsWritable, useTranslator } from '../store/useSettings';

/**
 * 外观 Tab（`01 §8.3`：名字 / 大小 / 透明度 / 皮肤 / 语言；本卡交付前四项 + 语言）。
 *
 * 说明（Tab 范围裁定）：`01 §8.3` 的「皮肤」属 `FR-7-5`（P1，依托骨骼插槽，随 S9 骨骼
 * 管线交付），本卡不提供该控件——没有插槽资源时给一个切不动的下拉框是假功能。
 *
 * 角色名（FR-7-1）经 `{name}` 占位渲染（C2）：本页输入框写的是**用户数据**
 * （存档 `pet.name`），文案里的名字一律走 `t(...)` + `{name}` 变量，绝不硬编码。
 */
export function AppearancePage(): React.ReactElement {
  const store = useSettingsStore();
  const { snapshot } = useSettingsState();
  const t = useTranslator();
  const disabled = !useSettingsWritable();

  const percent = (value: number): string => t('common.percent', { value: String(value) });

  return (
    <section className="dp-page" aria-label={t('app.tab.appearance')}>
      <div className="dp-row">
        <label className="dp-row-label" htmlFor="appearance-name">
          {t('appearance.name.label')}
        </label>
        <div className="dp-row-control dp-row-control-column">
          <input
            id="appearance-name"
            className="dp-input"
            type="text"
            value={snapshot.name}
            placeholder={t('appearance.name.placeholder')}
            disabled={disabled}
            maxLength={32}
            onChange={(event) => store.patch({ name: event.target.value })}
          />
          <span className="dp-row-hint">
            {t('appearance.name.hint', { name: snapshot.name || snapshot.defaultName })}
          </span>
        </div>
      </div>

      <Slider
        id="appearance-scale"
        label={t('appearance.scale.label')}
        value={snapshot.scalePercent}
        min={snapshot.scaleMinPercent}
        max={snapshot.scaleMaxPercent}
        step={snapshot.scaleStepPercent}
        display={percent(snapshot.scalePercent)}
        disabled={disabled}
        onChange={(value) => store.patch({ scalePercent: value })}
      />

      <Slider
        id="appearance-opacity"
        label={t('appearance.opacity.label')}
        value={snapshot.opacityPercent}
        min={snapshot.opacityMinPercent}
        max={snapshot.opacityMaxPercent}
        step={1}
        display={percent(snapshot.opacityPercent)}
        disabled={disabled}
        onChange={(value) => store.patch({ opacityPercent: value })}
      />

      <Segmented<Locale>
        legend={t('appearance.language.label')}
        value={resolveLocale(snapshot.language)}
        disabled={disabled}
        options={LOCALES.map((locale) => ({ value: locale, label: t(`appearance.language.${locale}`) }))}
        onChange={(value) => store.patch({ language: value })}
      />
    </section>
  );
}

export default AppearancePage;
