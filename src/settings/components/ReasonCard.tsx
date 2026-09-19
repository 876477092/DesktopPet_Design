import React from 'react';

import type { NeglectReasonV1, NeglectSnapshotV1, PetSnapshotV2 } from '@shared/ipc';

/**
 * 情绪原因卡（S10-M2；`01 §6.12` P 因子可解释性；FR-12-6）。
 *
 * 由桌面「长按心情条」入口唤起（`OverlayLayer.pressMoodBar/releaseMoodBar` 已在
 * S3-M5 落地，本卡只渲染 `pet://state.neglect` 的归因结果）。
 *
 * 纪律：本组件**不重算** P、不做业务判断——P 值 / 封顶 / 七因子权重 / 建议文案全部
 * 由 Rust `EmotionEngine` 算好后随快照下发（`neglect.reasons` 已按权重排序、
 * 附 `advice`）。前端只做展示与「按权重降序取前 N」的纯渲染。
 */
export interface ReasonCardProps {
  /** 实时快照（`null` = 未连接内核，不渲染）。 */
  snap: PetSnapshotV2 | null;
  /** 翻译函数。 */
  t: (key: string, vars?: Record<string, string>) => string;
  /** 关闭回调。 */
  onClose?: () => void;
}

/** 最多展示的归因条目（避免一屏压满；后端已排序）。 */
const MAX_REASONS = 4;

/** 取前 N 条有效归因（按后端下发顺序 = 权重降序）。 */
function topReasons(neglect: NeglectSnapshotV1): readonly NeglectReasonV1[] {
  return neglect.reasons.slice(0, MAX_REASONS);
}

/** 情绪原因卡（长按心情条弹出）。 */
export function ReasonCard({ snap, t, onClose }: ReasonCardProps): React.ReactElement | null {
  if (snap === null) {
    return null;
  }
  const { neglect, state } = snap;
  const reasons = topReasons(neglect);
  const pct = neglect.cap > 0 ? Math.min(100, (neglect.p / neglect.cap) * 100) : 0;

  return (
    <aside className="dp-reason-card" data-testid="reason-card" role="status" aria-live="polite">
      <header className="dp-reason-head">
        <span className="dp-reason-title">{t('reason.title')}</span>
        <span className="dp-reason-p">
          {t('reason.pressure', {
            p: String(Math.round(neglect.p)),
            cap: String(Math.round(neglect.cap)),
          })}
          （L{neglect.level}）
        </span>
        {onClose !== undefined && (
          <button type="button" className="dp-reason-close" onClick={onClose} data-testid="reason-close">
            ✕
          </button>
        )}
      </header>

      <div className="dp-needs-bar" role="presentation">
        <span className="dp-needs-bar-fill" style={{ width: `${pct}%` }} />
      </div>

      {reasons.length === 0 ? (
        <p className="dp-row-hint">{t('reason.none', { state })}</p>
      ) : (
        <ul className="dp-reason-list">
          {reasons.map((r) => (
            <li className="dp-reason-item" key={r.factorKey} data-testid={`reason-${r.factorKey}`}>
              <span className="dp-reason-label">{r.label}</span>
              <span className="dp-reason-weight">
                {t(r.dir === 'faster' ? 'reason.dir.faster' : 'reason.dir.slower', {
                  w: r.weight.toFixed(2),
                })}
              </span>
              {r.advice !== undefined && <p className="dp-reason-advice">{r.advice}</p>}
            </li>
          ))}
        </ul>
      )}
    </aside>
  );
}

export default ReasonCard;
