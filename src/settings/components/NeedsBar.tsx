import React from 'react';

/**
 * 单维属性条（S10-M1；`01 §6.12.6` 六维属性）。
 *
 * 纯展示：数值由实时快照 `pet://state.values` 注入，本组件不读时钟、不做业务计算。
 * 数值钳 [0,100] 兜底（脏数据不撑破条）；亲密度是等级制，由调用方传入已格式化文案。
 */
export interface NeedsBarProps {
  /** 维度名（已本地化）。 */
  label: string;
  /** 当前数值 0~100（条填充比例；越界钳制）。 */
  value: number;
  /** 右侧文案（缺省显示 `value%`；亲密度传 `Lv3·42/100` 等）。 */
  display?: string;
  /** 测试标识后缀。 */
  testId?: string;
}

/** 钳到 [0,100]。 */
function clamp0100(v: number): number {
  if (!Number.isFinite(v)) return 0;
  return Math.min(100, Math.max(0, v));
}

/** 单条属性（名 + 数值文案 + 填充条）。 */
export function NeedsBar({ label, value, display, testId }: NeedsBarProps): React.ReactElement {
  const pct = clamp0100(value);
  return (
    <li className="dp-needs-item" data-testid={testId ? `needs-${testId}` : 'needs-item'}>
      <span className="dp-needs-name">{label}</span>
      <span className="dp-needs-value" aria-hidden="true">
        {display ?? `${Math.round(pct)}%`}
      </span>
      <div className="dp-needs-bar" role="presentation">
        <span className="dp-needs-bar-fill" style={{ width: `${pct}%` }} />
      </div>
    </li>
  );
}

export default NeedsBar;
