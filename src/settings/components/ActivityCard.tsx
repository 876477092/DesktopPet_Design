import React from 'react';

import type { ActivitySnapshotV1 } from '@shared/ipc';

/**
 * 活动卡片（S10-M1；`01 §8.4`；`02 §5.13`）。
 *
 * 纯展示 + 一个「提前召回」动作：
 *   - 进行中（`running`）→ 进度条（`progressRatio`，core-loop 已结算）+ 剩余时间 + 召回钮；
 *   - 非进行中 → 显示当前阶段文案（等待派遣）。
 *
 * 本组件不读时钟、不算进度；进度与剩余全部来自 `pet://state.activity` 快照
 * （core-loop 以 wall-clock 结算，`02 §5.13` C3）。
 */
export interface ActivityCardProps {
  /** 实时活动快照（`null` = 尚未连接内核）。 */
  activity: ActivitySnapshotV1 | null;
  /** 「提前召回」回调（页层接 `invoke('pet_recall')`；纯展示层不直连 IPC，便于测试）。 */
  onRecall?: () => void;
  /** 翻译函数。 */
  t: (key: string, vars?: Record<string, string>) => string;
}

/** 毫秒 → `mm:ss`。 */
function formatRemaining(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '0:00';
  const totalSec = Math.round(ms / 1000);
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

/** 活动卡片（进行中进度 + 召回 / 空闲提示）。 */
export function ActivityCard({ activity, onRecall, t }: ActivityCardProps): React.ReactElement {
  if (activity === null || !activity.running || activity.instance === null) {
    return (
      <li className="dp-activity-card dp-activity-idle" data-testid="activity-card">
        <span className="dp-activity-name">{t('activity.idle.title')}</span>
        <span className="dp-activity-hint">{t('activity.idle.hint')}</span>
      </li>
    );
  }

  const inst = activity.instance;
  const pct = Math.min(1, Math.max(0, inst.progressRatio));
  return (
    <li className="dp-activity-card" data-testid="activity-card">
      <span className="dp-activity-name">{inst.defId}</span>
      <span className="dp-activity-hint">{t(`activity.kind.${inst.kind}`)}</span>
      <div className="dp-needs-bar" role="presentation">
        <span className="dp-needs-bar-fill" style={{ width: `${pct * 100}%` }} />
      </div>
      <span className="dp-activity-remaining">
        {t('activity.remaining', { ms: formatRemaining(inst.remainingMs) })}
      </span>
      {onRecall !== undefined && (
        <button
          type="button"
          className="dp-btn-secondary"
          onClick={onRecall}
          data-testid="activity-recall"
        >
          {t('activity.recall')}
        </button>
      )}
    </li>
  );
}

export default ActivityCard;
