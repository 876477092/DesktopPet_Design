import React from 'react';

import type { TopmostPolicy } from '@shared/ipc';

import Segmented from '../components/Segmented';
import Slider from '../components/Slider';
import Switch from '../components/Switch';
import { useSettingsState, useSettingsStore, useSettingsWritable, useTranslator } from '../store/useSettings';

/**
 * 行为 Tab（`01 §8.3`：自动走动 + 节奏 / 置顶 / 穿透 / 自启 / 音量静音 / 勿扰 / 提醒）。
 *
 * 分组：
 *   - 移动与节奏（`FR-7-4` 自动走动 + `01 §8.3` 节奏）；
 *   - 窗口行为（`FR-1-2` 置顶三态、`FR-1-6` 穿透、`FR-1-9` 开机自启）；
 *   - 声音（`FR-7-3` 音量 + 静音）；
 *   - 勿扰（`FR-10-4`）；
 *   - 提醒（`FR-10-2`，默认值来自 `schedule.json`——**取值域同样由快照下发**）。
 */
export function BehaviorPage(): React.ReactElement {
  const store = useSettingsStore();
  const { snapshot } = useSettingsState();
  const t = useTranslator();
  const disabled = !useSettingsWritable();

  const paceOptions = snapshot.roamPaceOptions.map((value) => ({
    value,
    label: paceLabel(value, snapshot.roamPaceOptions, t),
  }));

  return (
    <section className="dp-page" aria-label={t('app.tab.behavior')}>
      <Switch
        id="behavior-auto-roam"
        label={t('behavior.autoRoam.label')}
        checked={snapshot.autoRoam}
        disabled={disabled}
        onChange={(next) => store.patch({ autoRoam: next })}
      />

      <Segmented<number>
        legend={t('behavior.roamPace.label')}
        value={snapshot.roamPace}
        disabled={disabled}
        options={paceOptions}
        onChange={(value) => store.patch({ roamPace: value })}
      />

      <Segmented<TopmostPolicy>
        legend={t('behavior.topmost.label')}
        value={snapshot.alwaysOnTopPolicy}
        disabled={disabled}
        options={[
          { value: 'Always', label: t('behavior.topmost.always') },
          { value: 'BelowFullscreen', label: t('behavior.topmost.belowFullscreen') },
          { value: 'Never', label: t('behavior.topmost.never') },
        ]}
        onChange={(value) => store.patch({ alwaysOnTopPolicy: value })}
      />

      <Switch
        id="behavior-click-through"
        label={t('behavior.clickThrough.label')}
        checked={snapshot.clickThrough}
        disabled={disabled}
        hint={t('behavior.clickThrough.hint')}
        onChange={(next) => store.patch({ clickThrough: next })}
      />

      <Switch
        id="behavior-autostart"
        label={t('behavior.autostart.label')}
        checked={snapshot.autostart}
        disabled={disabled}
        onChange={(next) => store.patch({ autostart: next })}
      />

      <Slider
        id="behavior-volume"
        label={t('audio.volume.label')}
        value={snapshot.masterVolumePercent}
        min={snapshot.volumeMinPercent}
        max={snapshot.volumeMaxPercent}
        step={1}
        display={t('common.percent', { value: String(snapshot.masterVolumePercent) })}
        disabled={disabled}
        onChange={(value) => store.patch({ masterVolumePercent: value })}
      />

      <Switch
        id="behavior-muted"
        label={t('audio.muted.label')}
        checked={snapshot.muted}
        disabled={disabled}
        onChange={(next) => store.patch({ muted: next })}
      />

      <h3 className="dp-section-title">{t('reminder.section')}</h3>

      <Switch
        id="behavior-dnd"
        label={t('behavior.dnd.label')}
        checked={snapshot.doNotDisturb}
        disabled={disabled}
        hint={t('behavior.dnd.hint')}
        onChange={(next) => store.patch({ doNotDisturb: next })}
      />

      <Switch
        id="reminder-sedentary-enabled"
        label={t('reminder.sedentary.label')}
        checked={snapshot.reminders.sedentaryEnabled}
        disabled={disabled}
        onChange={(next) =>
          store.patch({ reminders: { sedentaryEnabled: next } })
        }
      />

      <Slider
        id="reminder-sedentary-interval"
        label={t('reminder.sedentary.interval')}
        value={snapshot.reminders.sedentaryIntervalMin}
        min={snapshot.reminders.intervalMinMin}
        max={snapshot.reminders.intervalMaxMin}
        step={5}
        display={t('reminder.minutes', { value: String(snapshot.reminders.sedentaryIntervalMin) })}
        disabled={disabled || !snapshot.reminders.sedentaryEnabled}
        onChange={(value) => store.patch({ reminders: { sedentaryIntervalMin: value } })}
      />

      <Switch
        id="reminder-water-enabled"
        label={t('reminder.water.label')}
        checked={snapshot.reminders.waterEnabled}
        disabled={disabled}
        onChange={(next) => store.patch({ reminders: { waterEnabled: next } })}
      />

      <Slider
        id="reminder-water-interval"
        label={t('reminder.water.interval')}
        value={snapshot.reminders.waterIntervalMin}
        min={snapshot.reminders.intervalMinMin}
        max={snapshot.reminders.intervalMaxMin}
        step={5}
        display={t('reminder.minutes', { value: String(snapshot.reminders.waterIntervalMin) })}
        disabled={disabled || !snapshot.reminders.waterEnabled}
        onChange={(value) => store.patch({ reminders: { waterIntervalMin: value } })}
      />

      <p className="dp-row-hint">{t('reminder.ackResets.label')}</p>
    </section>
  );
}

/**
 * 节奏档位的本地化标签：按**档位在候选数组中的位置**取名（首/中/末 → 慢/正常/快），
 * 而不是按数值硬编码——换了 `settings.json.roam.paceOptions` 后文案仍正确。
 */
function paceLabel(
  value: number,
  options: readonly number[],
  t: (key: string) => string,
): string {
  const index = options.indexOf(value);
  if (options.length >= 3 && index === 0) {
    return t('behavior.roamPace.slow');
  }
  if (index === options.length - 1) {
    return t('behavior.roamPace.fast');
  }
  return t('behavior.roamPace.normal');
}

export default BehaviorPage;
