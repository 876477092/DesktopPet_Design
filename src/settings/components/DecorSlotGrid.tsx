import React, { useState } from 'react';

/**
 * 桌面装饰 5 槽摆放格（S10-M1；`01 FR-13-6`；`02 §5` DECOR_SLOTS=5）。
 *
 * 读 `pet://state.decor`（长度恒 5，空位 null，占用为商品 ID）；写经
 * `pet_decor_place` / `pet_decor_remove`（core-loop 是存档唯一写者，前端只投递）。
 *
 * 交互：左侧列出**已拥有的可摆放摆件**（背包 ∩ furniture 目录），选中一个 →
 * 点右侧空格放入；点已占用槽 → 取下。槽位非法越界由后端再校验。
 */
export interface OwnedDecor {
  readonly itemId: string;
  readonly name: string;
}

export interface DecorSlotGridProps {
  /** 当前 5 槽（空位 null）。 */
  decor: readonly (string | null)[];
  /** 已拥有的可摆放摆件。 */
  owned: readonly OwnedDecor[];
  /** 翻译函数。 */
  t: (key: string, vars?: Record<string, string>) => string;
  /** 放置回调（页层接 IPC）。 */
  onPlace?: (slot: number, itemId: string) => void;
  /** 取下回调。 */
  onRemove?: (slot: number) => void;
}

/** 固定 5 槽（与 `DECOR_SLOTS=5` 同值；前端只展示，不计算）。 */
const SLOT_COUNT = 5;

/** 装饰摆放格（选品列 + 5 槽）。 */
export function DecorSlotGrid({
  decor,
  owned,
  t,
  onPlace,
  onRemove,
}: DecorSlotGridProps): React.ReactElement {
  const [selected, setSelected] = useState<string | null>(null);

  const slots: (string | null)[] = Array.from({ length: SLOT_COUNT }, (_, i) => decor[i] ?? null);

  return (
    <div className="dp-decor" data-testid="decor-slot-grid">
      <div className="dp-decor-pick">
        <span className="dp-section-title">{t('decor.pick.title')}</span>
        {owned.length === 0 && <p className="dp-row-hint">{t('decor.pick.empty')}</p>}
        {owned.map((item) => (
          <button
            key={item.itemId}
            type="button"
            className={`dp-decor-item ${selected === item.itemId ? 'is-selected' : ''}`}
            data-testid={`decor-item-${item.itemId}`}
            onClick={() => setSelected(selected === item.itemId ? null : item.itemId)}
          >
            {item.name}
          </button>
        ))}
      </div>

      <div className="dp-decor-slots" role="group" aria-label={t('decor.slots.title')}>
        {slots.map((occupant, slot) => {
          const label = occupant ?? t('decor.slot.empty', { n: String(slot + 1) });
          const clickable =
            onPlace !== undefined && occupant === null && selected !== null;
          return (
            <button
              key={slot}
              type="button"
              className={`dp-decor-slot ${occupant !== null ? 'is-occupied' : ''}`}
              data-testid={`decor-slot-${slot}`}
              onClick={() => {
                if (occupant !== null) {
                  onRemove?.(slot);
                } else if (clickable && selected !== null) {
                  onPlace?.(slot, selected);
                }
              }}
            >
              {label}
            </button>
          );
        })}
      </div>
    </div>
  );
}

export default DecorSlotGrid;
