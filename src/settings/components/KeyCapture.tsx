import React, { useState } from 'react';

/**
 * 快捷键捕获（`03 S5-M3` 交付物 `components/KeyCapture.tsx`）。
 *
 * ## 当前消费方（登记，避免「无消费方」被误判为死代码）
 * T-15（S5-M3/M4/M5）**没有**任何一个设置项需要快捷键：`01 §6.7 FR-7` 的十项设置
 * （命名 / 缩放 / 音量 / 开关 / 皮肤 / 透明度 / 语言 / 多宠物 / 敏感度 / 口头禅）全为
 * 滑杆与开关。快捷键的第一个消费方是 **FR-10-3 自定义提醒**（P2 已延后，不排期）
 * 与后续「唤醒/暂停」类快捷操作。故本组件按卡片交付物清单**独立交付并单测覆盖**，
 * 不在本卡接线到任何设置项——否则等于「新增未定义设置项」（`03 S5-M3` 明确禁止）。
 *
 * ## 语义
 * - 按键组合规范化：修饰键固定顺序 `Ctrl+Alt+Shift+Meta` + 主键（去修饰）；
 * - 纯修饰键（只按 Ctrl）**不产生组合**（避免捕获到半截组合）；
 * - `Esc` 清空（与「取消」习惯一致）；`Backspace`/`Delete` 亦清空；
 * - `preventDefault` 只在本组件聚焦时生效（不吞全局快捷键）。
 */

/** 修饰键显示名（与 Windows 习惯一致；纯展示，不参与业务）。 */
const MODIFIER_LABELS: ReadonlyArray<readonly [keyof ModifierState, string]> = [
  ['ctrl', 'Ctrl'],
  ['alt', 'Alt'],
  ['shift', 'Shift'],
  ['meta', 'Meta'],
];

/** 修饰键按下状态。 */
export interface ModifierState {
  /** Ctrl。 */
  readonly ctrl: boolean;
  /** Alt。 */
  readonly alt: boolean;
  /** Shift。 */
  readonly shift: boolean;
  /** Meta / Win。 */
  readonly meta: boolean;
}

/** 由键盘事件解析修饰键状态（纯函数，可单测）。 */
export function modifiersOf(event: Pick<KeyboardEvent, 'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey'>): ModifierState {
  return {
    ctrl: event.ctrlKey,
    alt: event.altKey,
    shift: event.shiftKey,
    meta: event.metaKey,
  };
}

/** 主键是否可用作组合键主体（纯修饰键不可）。 */
export function isModifierKey(key: string): boolean {
  return key === 'Control' || key === 'Alt' || key === 'Shift' || key === 'Meta' || key === 'OS';
}

/**
 * 把一次按键规范化为组合字符串（纯函数，可单测）。
 *
 * @returns 组合串（如 `Ctrl+Shift+K`）；纯修饰键 → `null`。
 */
export function formatCombo(
  event: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey'>,
): string | null {
  if (isModifierKey(event.key)) {
    return null;
  }
  const mods = modifiersOf(event);
  const parts = MODIFIER_LABELS.filter(([flag]) => mods[flag]).map(([, label]) => label);
  parts.push(normalizeKey(event.key));
  return parts.join('+');
}

/** 主键归一化（单字符大写 / 空格与方向键可见化）。 */
export function normalizeKey(key: string): string {
  if (key === ' ') {
    return 'Space';
  }
  if (key.length === 1) {
    return key.toUpperCase();
  }
  return key;
}

/** 清空键（与「取消」习惯一致）。 */
const CLEAR_KEYS: readonly string[] = ['Escape', 'Backspace', 'Delete'];

/** 快捷键捕获属性。 */
export interface KeyCaptureProps {
  /** 已本地化标签。 */
  readonly label: string;
  /** 当前组合（`null` = 未设置）。 */
  readonly value: string | null;
  /** 只读预览。 */
  readonly disabled?: boolean;
  /** 已本地化提示。 */
  readonly hint?: string;
  /** 未设置时的占位文案（已本地化）。 */
  readonly placeholder: string;
  /** 清空按钮文案（已本地化）。 */
  readonly clearLabel: string;
  /** 变更回调（`null` = 清空）。 */
  readonly onChange: (combo: string | null) => void;
}

/** 快捷键捕获。 */
export function KeyCapture({
  label,
  value,
  disabled = false,
  hint,
  placeholder,
  clearLabel,
  onChange,
}: KeyCaptureProps): React.ReactElement {
  const [capturing, setCapturing] = useState(false);

  const onKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>): void => {
    if (disabled) {
      return;
    }
    event.preventDefault();
    if (CLEAR_KEYS.includes(event.key)) {
      onChange(null);
      setCapturing(false);
      return;
    }
    const combo = formatCombo(event.nativeEvent);
    if (combo !== null) {
      onChange(combo);
      setCapturing(false);
    }
  };

  return (
    <div className="dp-row">
      <span className="dp-row-label">{label}</span>
      <div className="dp-row-control dp-row-control-inline">
        <button
          type="button"
          className={`dp-keycapture${capturing ? ' dp-keycapture-on' : ''}`}
          aria-label={label}
          disabled={disabled}
          data-testid="key-capture"
          onClick={() => setCapturing(true)}
          onBlur={() => setCapturing(false)}
          onKeyDown={onKeyDown}
        >
          {value ?? placeholder}
        </button>
        <button
          type="button"
          className="dp-btn dp-btn-ghost"
          disabled={disabled || value === null}
          onClick={() => onChange(null)}
        >
          {clearLabel}
        </button>
        {hint !== undefined && <span className="dp-row-hint">{hint}</span>}
      </div>
    </div>
  );
}

export default KeyCapture;
