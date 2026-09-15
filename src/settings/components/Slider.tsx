import React from 'react';

/**
 * 滑块控件（`03 S5-M3` 交付物；`01 FR-7-2` 大小 / `FR-7-3` 音量 / `FR-7-6` 透明度）。
 *
 * 纪律（`03 S5-M3`「禁止顺手改动：不在 UI 硬编码数值」）：
 * - `min` / `max` / `step` **必须**由调用方从设置快照透传（快照又来自 `settings.json`）；
 * - `display` 为**已本地化**的显示值（如 `100%` / `45 分钟`），组件不自行拼接单位
 *   （单位文案属 i18n 域，组件保持纯展示）；
 * - 组件**不读时钟、不发 IPC**：只把 `onChange` 交给上层（上层走防抖提交）。
 */
export interface SliderProps {
  /** 关联 label 的 id（无障碍：`htmlFor` / `id` 同源）。 */
  readonly id: string;
  /** 已本地化标签。 */
  readonly label: string;
  /** 当前值。 */
  readonly value: number;
  /** 下界（来自配置，不硬编码）。 */
  readonly min: number;
  /** 上界（来自配置）。 */
  readonly max: number;
  /** 步进（来自配置）。 */
  readonly step: number;
  /** 已本地化显示值。 */
  readonly display: string;
  /** 只读预览（设置服务不可写）。 */
  readonly disabled?: boolean;
  /** 值变化回调（输入即回调，防抖由上层负责）。 */
  readonly onChange: (value: number) => void;
}

/** 滑块（`<input type="range">` + 数值回显）。 */
export function Slider({
  id,
  label,
  value,
  min,
  max,
  step,
  display,
  disabled = false,
  onChange,
}: SliderProps): React.ReactElement {
  return (
    <div className="dp-row">
      <label className="dp-row-label" htmlFor={id}>
        {label}
      </label>
      <div className="dp-row-control">
        <input
          id={id}
          className="dp-slider"
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          disabled={disabled}
          aria-valuemin={min}
          aria-valuemax={max}
          aria-valuenow={value}
          aria-valuetext={display}
          onChange={(event) => onChange(Number(event.target.value))}
        />
        <span className="dp-row-value" aria-hidden="true">
          {display}
        </span>
      </div>
    </div>
  );
}

export default Slider;
