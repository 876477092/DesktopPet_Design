import React from 'react';

import type { CatchphraseFrequency } from '@shared/ipc';

import Segmented from '../components/Segmented';
import Switch from '../components/Switch';
import { useSettingsState, useSettingsStore, useSettingsWritable, useTranslator } from '../store/useSettings';

/**
 * 互动 Tab（`01 §8.3`：黏人程度 / 口头禅「心心」开关与频率；+ `FR-7-4` 点击微反馈、
 * `Q-18` 活跃感知、`Q-E` 轻松模式）。
 *
 * 敏感度档位（`FR-7-9 / FR-11-11`）与口头禅频率（L-03）的**候选值与当前值均来自快照**
 * （快照又来自 `settings.json.emotion.sensitivityOptions` / `character.json`），
 * 本页不含任何数值字面量。
 */
export function InteractionPage(): React.ReactElement {
  const store = useSettingsStore();
  const { snapshot } = useSettingsState();
  const t = useTranslator();
  const disabled = !useSettingsWritable();

  const sensitivityOptions = snapshot.sensitivityOptions.map((value) => ({
    value,
    label: sensitivityLabel(value, snapshot.sensitivityOptions, t),
  }));

  return (
    <section className="dp-page" aria-label={t('app.tab.interaction')}>
      <Segmented<number>
        legend={t('interaction.sensitivity.label')}
        value={snapshot.sensitivityValue}
        disabled={disabled}
        options={sensitivityOptions}
        onChange={(value) => store.patch({ sensitivityValue: value })}
      />

      <h3 className="dp-section-title">{t('interaction.catchphrase.enabled')}</h3>

      <Switch
        id="interaction-catchphrase-enabled"
        label={t('interaction.catchphrase.enabled')}
        checked={snapshot.catchphraseEnabled}
        disabled={disabled}
        onChange={(next) => store.patch({ catchphraseEnabled: next })}
      />

      <Segmented<CatchphraseFrequency>
        legend={t('interaction.catchphrase.frequency.label')}
        value={snapshot.catchphraseFrequency}
        disabled={disabled || !snapshot.catchphraseEnabled}
        options={[
          { value: 'off', label: t('interaction.catchphrase.frequency.off') },
          { value: 'low', label: t('interaction.catchphrase.frequency.low') },
          { value: 'standard', label: t('interaction.catchphrase.frequency.standard') },
          { value: 'high', label: t('interaction.catchphrase.frequency.high') },
        ]}
        onChange={(value) => store.patch({ catchphraseFrequency: value })}
      />

      <h3 className="dp-section-title">{t('interaction.clickFeedback.label')}</h3>

      <Switch
        id="interaction-easy-coax"
        label={t('interaction.easyCoax.label')}
        checked={snapshot.easyCoaxMode}
        disabled={disabled}
        hint={t('interaction.easyCoax.hint')}
        onChange={(next) => store.patch({ easyCoaxMode: next })}
      />

      <Switch
        id="interaction-click-feedback"
        label={t('interaction.clickFeedback.label')}
        checked={snapshot.clickFeedbackEnabled}
        disabled={disabled}
        onChange={(next) => store.patch({ clickFeedbackEnabled: next })}
      />

      <Switch
        id="interaction-activity-sensing"
        label={t('interaction.privacy.label')}
        checked={snapshot.activitySensing}
        disabled={disabled}
        hint={t('interaction.privacy.hint')}
        onChange={(next) => store.patch({ activitySensing: next })}
      />
    </section>
  );
}

/** 敏感度档位文案：按候选值相对位置取名（首/中/末 → 慢热/刚刚好/超黏人）。 */
function sensitivityLabel(
  value: number,
  options: readonly number[],
  t: (key: string) => string,
): string {
  const index = options.indexOf(value);
  const mid = (options.length - 1) / 2;
  if (options.length >= 3 && index < mid) {
    return t('interaction.sensitivity.relaxed');
  }
  if (options.length >= 3 && index > mid) {
    return t('interaction.sensitivity.clingy');
  }
  return t('interaction.sensitivity.normal');
}

export default InteractionPage;
