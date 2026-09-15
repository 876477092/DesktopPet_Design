import React from 'react';

/**
 * 开关（`03 S5-M3` 交付物；`01 FR-7-4` 自动走动 / 勿扰 / 穿透 / 置顶、`FR-1-9` 开机自启）。
 *
 * 用 `role="switch"` + `aria-checked`（比 checkbox 更贴近「立即生效的开关」语义），
 * 同时用 `onClick` 触发（按钮语义天然键盘可达：Enter/Space 均触发 click）。
 * `hint` 为可选补充说明（如「开启后托盘『摸摸』仍可用」）。
 */
export interface SwitchProps {
  /** 元素 id（与 label 的 `htmlFor` 同源；同时用作 `data-testid` 锚点）。 */
  readonly id: string;
  /** 已本地化标签。 */
  readonly label: string;
  /** 当前状态。 */
  readonly checked: boolean;
  /** 只读预览。 */
  readonly disabled?: boolean;
  /** 补充说明（可选）。 */
  readonly hint?: string;
  /** 切换回调（传入目标态，而非「翻转」——上层无需知道当前值）。 */
  readonly onChange: (next: boolean) => void;
}

/** 开关。 */
export function Switch({
  id,
  label,
  checked,
  disabled = false,
  hint,
  onChange,
}: SwitchProps): React.ReactElement {
  return (
    <div className="dp-row">
      <label className="dp-row-label" htmlFor={id}>
        {label}
      </label>
      <div className="dp-row-control dp-row-control-inline">
        <button
          id={id}
          type="button"
          role="switch"
          aria-checked={checked}
          disabled={disabled}
          className={`dp-switch${checked ? ' dp-switch-on' : ''}`}
          onClick={() => onChange(!checked)}
        >
          <span className="dp-switch-knob" aria-hidden="true" />
        </button>
        {hint !== undefined && <span className="dp-row-hint">{hint}</span>}
      </div>
    </div>
  );
}

export default Switch;
