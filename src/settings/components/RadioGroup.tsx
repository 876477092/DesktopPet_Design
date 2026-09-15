import React from 'react';

/**
 * 单选组（`03 S5-M3` 交付物；`01 §8.3` 皮肤 / 黏人程度 / 口头禅频率等枚举项）。
 *
 * 用原生 `<input type="radio">`：键盘可达、屏幕阅读器语义免费获得；外观由
 * `settings.css` 的 `.dp-radio*` 覆盖。与 [`./Segmented`] 的差别是「表单语义 vs 紧凑分段」，
 * 二者都保留（配置项多时紧凑版更省竖向空间）。
 */
export interface RadioOption<T extends string | number> {
  /** 选项值（直接回传给 `onChange`）。 */
  readonly value: T;
  /** 已本地化选项文案。 */
  readonly label: string;
}

/** 单选组属性。 */
export interface RadioGroupProps<T extends string | number> {
  /** 组标题（`<fieldset>` 的 legend）。 */
  readonly legend: string;
  /** 组内 name（同一组必须唯一且一致）。 */
  readonly name: string;
  /** 当前值。 */
  readonly value: T;
  /** 候选值（顺序即显示顺序）。 */
  readonly options: readonly RadioOption<T>[];
  /** 只读预览。 */
  readonly disabled?: boolean;
  /** 选择回调。 */
  readonly onChange: (value: T) => void;
}

/** 单选组（原生 radio + fieldset 语义）。 */
export function RadioGroup<T extends string | number>({
  legend,
  name,
  value,
  options,
  disabled = false,
  onChange,
}: RadioGroupProps<T>): React.ReactElement {
  return (
    <fieldset className="dp-row dp-row-block" disabled={disabled}>
      <legend className="dp-row-label">{legend}</legend>
      <div className="dp-radio-group">
        {options.map((option) => {
          const id = `${name}-${String(option.value)}`;
          const checked = option.value === value;
          return (
            <label className="dp-radio" key={id} htmlFor={id}>
              <input
                id={id}
                type="radio"
                name={name}
                value={String(option.value)}
                checked={checked}
                disabled={disabled}
                onChange={() => onChange(option.value)}
              />
              <span>{option.label}</span>
            </label>
          );
        })}
      </div>
    </fieldset>
  );
}

export default RadioGroup;
