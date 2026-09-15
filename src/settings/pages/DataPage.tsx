import React, { useEffect, useMemo, useState } from 'react';

import type { SaveBackupV1 } from '@shared/ipc';

import ConfirmDialog from '../components/ConfirmDialog';
import { useSettingsState, useSettingsStore, useSettingsWritable, useTranslator } from '../store/useSettings';

/**
 * 数据 Tab（`01 FR-8-2 / FR-8-4`；`03 S5-M1` 裁定 ④ 指定的**存档提示承载面**）。
 *
 * ## 为什么本页是 AC-14「提示」的落点
 * `02 §5 K-7` 要求损坏档「重建默认档**并提示**」。`pet://` 事件面没有「存档状态」事件、
 * 托盘也没有通知 API，因此提示必须由一处真实 UI 承载——即本页：挂载时经
 * `save_status` 命令取回加载落点，`needsNotice` 为真时显示醒目提示条，并列出候选备份
 * 供「导入恢复」。S5-M1 登记的两条下游归口（S5-M3 数据 Tab / S6-M2 自愈巡检）中，
 * **本卡闭环前者**；S6-M2 的**主动**提示（未打开设置页时）仍待其交付。
 *
 * ## 破坏性动作
 * 「重置全部数据」与「导入存档」都会重启进程，故一律走 [`ConfirmDialog`] 二次确认。
 */
export function DataPage(): React.ReactElement {
  const store = useSettingsStore();
  const { saveStatus, error, snapshot } = useSettingsState();
  const t = useTranslator();
  const writable = useSettingsWritable();
  const [confirming, setConfirming] = useState<'reset' | null>(null);
  const [pendingImport, setPendingImport] = useState<SaveBackupV1 | null>(null);

  useEffect(() => {
    void store.refreshSaveStatus();
  }, [store]);

  const stateKey = useMemo(() => saveStateKey(saveStatus?.state ?? 'unknown'), [saveStatus]);
  const importable = saveStatus?.backups.filter((item) => item.importable) ?? [];

  return (
    <section className="dp-page" aria-label={t('app.tab.data')}>
      <h3 className="dp-section-title">{t('data.save.section')}</h3>

      {saveStatus?.needsNotice === true && (
        <p className="dp-notice dp-notice-warn" data-testid="save-notice">
          {t(stateKey)}
        </p>
      )}

      <div className="dp-kv">
        <span className="dp-kv-key">{t('data.save.path')}</span>
        <span className="dp-kv-value" data-testid="save-path">
          {saveStatus?.path ?? t('common.unavailable')}
        </span>
      </div>
      <div className="dp-kv">
        <span className="dp-kv-key">{t('app.saved.hint')}</span>
        <span className="dp-kv-value">
          {saveStatus?.writable === true ? t('data.save.writable') : t('data.save.readonly')}
        </span>
      </div>
      <div className="dp-kv">
        <span className="dp-kv-key">{t('data.save.state.loaded')}</span>
        <span className="dp-kv-value">{t(stateKey)}</span>
      </div>

      <h3 className="dp-section-title">{t('data.save.backups')}</h3>
      <p className="dp-row-hint">{t('data.save.import.hint')}</p>
      {importable.length === 0 ? (
        <p className="dp-row-hint" data-testid="no-backup">
          {t('data.save.import.none')}
        </p>
      ) : (
        <ul className="dp-backup-list">
          {importable.map((backup) => (
            <li className="dp-backup-item" key={backup.file}>
              <span className="dp-backup-name" title={backup.file}>
                {backup.file}
              </span>
              <span className="dp-backup-size">{formatBytes(backup.sizeBytes)}</span>
              <button
                type="button"
                className="dp-btn"
                disabled={!writable}
                onClick={() => setPendingImport(backup)}
              >
                {t('data.save.import')}
              </button>
            </li>
          ))}
        </ul>
      )}

      <h3 className="dp-section-title">{t('data.session.section')}</h3>
      <div className="dp-button-row">
        <button
          type="button"
          className="dp-btn"
          disabled={!writable}
          onClick={() => void store.emotionCommand('reset')}
        >
          {t('data.session.reset')}
        </button>
        <span className="dp-row-hint">{t('data.session.reset.hint')}</span>
      </div>
      <div className="dp-button-row">
        <button
          type="button"
          className="dp-btn"
          disabled={!writable}
          onClick={() => void store.emotionCommand('recall')}
        >
          {t('data.session.recall', { name: snapshot.name || snapshot.defaultName })}
        </button>
      </div>

      <h3 className="dp-section-title">{t('data.reset.section')}</h3>
      <button
        type="button"
        className="dp-btn dp-btn-danger"
        disabled={!writable || saveStatus?.available !== true}
        data-testid="reset-all"
        onClick={() => setConfirming('reset')}
      >
        {t('data.reset.label')}
      </button>

      {error !== null && <p className="dp-notice dp-notice-error">{error}</p>}

      <ConfirmDialog
        open={confirming === 'reset'}
        title={t('data.reset.confirm.title')}
        body={t('data.reset.confirm.body')}
        confirmLabel={t('common.confirm')}
        cancelLabel={t('common.cancel')}
        onCancel={() => setConfirming(null)}
        onConfirm={() => {
          setConfirming(null);
          void store.resetAll();
        }}
      />

      <ConfirmDialog
        open={pendingImport !== null}
        title={t('data.save.import')}
        body={pendingImport?.file ?? ''}
        confirmLabel={t('common.confirm')}
        cancelLabel={t('common.cancel')}
        onCancel={() => setPendingImport(null)}
        onConfirm={() => {
          const backup = pendingImport;
          setPendingImport(null);
          if (backup !== null) {
            void store.importSave(backup.file);
          }
        }}
      />
    </section>
  );
}

/** 存档状态词表名 → i18n 键（未知状态回退「已隔离」文案，保守提示用户）。 */
export function saveStateKey(state: string): string {
  switch (state) {
    case 'fresh':
      return 'data.save.state.fresh';
    case 'loaded':
      return 'data.save.state.loaded';
    case 'recoveredFromBak':
      return 'data.save.state.recovered';
    case 'isolatedCorrupt':
      return 'data.save.state.isolated';
    case 'isolatedFuture':
      return 'data.save.state.future';
    case 'migrationPending':
      return 'data.save.state.migrationPending';
    default:
      return 'data.save.state.isolated';
  }
}

/** 字节数 → 可读体积（KB / MB；用于备份列表）。 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default DataPage;
