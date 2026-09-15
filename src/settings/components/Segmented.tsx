import React from 'react';

/**
 * 分段控件（`03 S5-M3` 交付物；`01 §8.3` 「节奏 (●正常)」「她的黏人程度」等紧凑枚举项）。
 *
 * 与 [`./RadioGroup`] 的取舍：分段控件用 `role="radiogroup"` + 按钮组，横向省空间、
 * 视觉更贴近设计稿的「(●正常)」；RadioGroup 保留为表单语义版（配置项多时用）。
 * 两者都**不硬编码候选值**：`options` 由调用方从设置快照透传。
 */
export interface SegmentedOption<T extends string | number> {
  /** 选项值。 */
  readonly value: T;
  /** 已本地化选项文案。 */
  readonly label: string;
}

/** 分段控件属性。 */
export interface SegmentedProps<T extends string | number> {
  /** 组标签（`aria-label`；同时作为可见标题）。 */
  readonly legend: string;
  /** 当前值。 */
  readonly value: T;
  /** 候选值（顺序即显示顺序）。 */
  readonly options: readonly SegmentedOption<T>[];
  /** 只读预览。 */
  readonly disabled?: boolean;
  /** 选择回调。 */
  readonly onChange: (value: T) => void;
}

/** 分段控件。 */
export function Segmented<T extends string | number>({
  legend,
  value,
  options,
  disabled = false,
  onChange,
}: SegmentedProps<T>): React.ReactElement {
  return (
    <div className="dp-row dp-row-block">
      <span className="dp-row-label" id={`${legend}-label`}>
        {legend}
      </span>
      <div
        className="dp-segmented"
        role="radiogroup"
        aria-labelledby={`${legend}-label`}
        data-testid={`segmented-${legend}`}
      >
        {options.map((option) => {
          const selected = option.value === value;
          return (
            <button
              key={String(option.value)}
              type="button"
              role="radio"
              aria-checked={selected}
              disabled={disabled}
              className={`dp-segment${selected ? ' dp-segment-on' : ''}`}
              onClick={() => onChange(option.value)}
            >
              {option.label}
            </button>
          );
        })}
      </div>
    </div>
  );
}

export default Segmented;
