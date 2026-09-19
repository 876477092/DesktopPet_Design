import React, { useCallback } from 'react';

import { invokeCommand } from '@shared/ipc';
import { usePetSnapshot } from '../hooks/usePetSnapshot';
import { useTranslator } from '../store/useSettings';
import ActivityCard from '../components/ActivityCard';
import { DISPATCH_OPTIONS, type DispatchOption } from '@shared/activityCatalog';

/**
 * 活动 Tab（`01 §8.4` 打工 / 学习 / 旅游；S10-M1）。
 *
 * - 进行中：`ActivityCard` 显示进度 + 提前召回（`pet_recall`）；
 * - 空闲：三类派遣按钮（`pet_dispatch`，kind/defId/durationMin 全部来自
 *   `activities.json` 静态目录，不在 UI 写死）。
 *
 * 前置校验（L4/L5 / 体力 / 饱洁 / 当日次数 / 安静时段）在 Rust core-loop 侧完成；
 * 拒绝只记日志，快照不切活动态——本页不预判、不阻断。
 */
export function ActivityPage(): React.ReactElement {
  const t = useTranslator();
  const snap = usePetSnapshot();

  const dispatch = useCallback((opt: DispatchOption, durationMin: number) => {
    void invokeCommand('pet_dispatch', {
      kind: opt.kind,
      defId: opt.defId,
      durationMin,
    }).catch(() => {
      // 派遣被拒（体力不足 / 安静时段 / 当日次数已满）→ core-loop 已记日志；
      // 前端不弹窗（设置窗是只读监控面，拒绝反馈走桌面气泡）。
    });
  }, []);

  const recall = useCallback(() => {
    void invokeCommand('pet_recall').catch(() => undefined);
  }, []);

  const running = snap?.activity?.running ?? false;

  return (
    <section className="dp-page" aria-label={t('app.tab.activity')}>
      <ul className="dp-activity-list">
        <ActivityCard activity={snap?.activity ?? null} onRecall={running ? recall : undefined} t={t} />
      </ul>

      {!running && (
        <div className="dp-dispatch-group">
          {(['work', 'study', 'travel'] as const).map((kind) => {
            const options = DISPATCH_OPTIONS.filter((o) => o.kind === kind);
            if (options.length === 0) return null;
            return (
              <div className="dp-dispatch-row" key={kind}>
                <span className="dp-dispatch-kind">{t(`activity.kind.${kind}`)}</span>
                {options.map((opt) =>
                  opt.durations.map((dur) => (
                    <button
                      key={`${opt.defId}-${dur}`}
                      type="button"
                      className="dp-btn-secondary"
                      data-testid={`dispatch-${opt.defId}-${dur}`}
                      onClick={() => dispatch(opt, dur)}
                    >
                      {opt.name} · {dur}min
                    </button>
                  )),
                )}
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

export default ActivityPage;
