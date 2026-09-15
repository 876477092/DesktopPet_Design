import React, { useEffect, useRef } from 'react';

/**
 * 二次确认对话框（`03 S5-M3` 交付物；`01 FR-8-4`「重置全部数据」二次确认）。
 *
 * 用自绘遮罩 + `role="dialog"`（不用 `<dialog>`：其 `showModal()` 在旧 WebView2 与
 * jsdom 下行为不一致，自绘层可预测且易于单测）。
 *
 * 无障碍与安全约定：
 * - 打开时把焦点移到**取消**按钮（破坏性动作不默认聚焦确认键）；
 * - `Esc` 关闭（等同取消）；遮罩点击等同取消；
 * - 关闭态**不渲染任何节点**（避免不可见按钮仍可被 Tab 到）。
 */
export interface ConfirmDialogProps {
  /** 是否打开。 */
  readonly open: boolean;
  /** 已本地化标题。 */
  readonly title: string;
  /** 已本地化正文（可含换行）。 */
  readonly body: string;
  /** 已本地化确认文案。 */
  readonly confirmLabel: string;
  /** 已本地化取消文案。 */
  readonly cancelLabel: string;
  /** 确认回调。 */
  readonly onConfirm: () => void;
  /** 取消回调。 */
  readonly onCancel: () => void;
}

/** 二次确认对话框。 */
export function ConfirmDialog({
  open,
  title,
  body,
  confirmLabel,
  cancelLabel,
  onConfirm,
  onCancel,
}: ConfirmDialogProps): React.ReactElement | null {
  const cancelRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }
    // 焦点落到「取消」：破坏性动作不得默认聚焦确认按钮。
    cancelRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') {
        onCancel();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [open, onCancel]);

  if (!open) {
    return null;
  }

  return (
    <div className="dp-modal-mask" role="presentation" onClick={onCancel}>
      <div
        className="dp-modal"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        data-testid="confirm-dialog"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="dp-modal-title">{title}</h2>
        <p className="dp-modal-body">{body}</p>
        <div className="dp-modal-actions">
          <button ref={cancelRef} type="button" className="dp-btn" onClick={onCancel}>
            {cancelLabel}
          </button>
          <button
            type="button"
            className="dp-btn dp-btn-danger"
            data-testid="confirm-dialog-accept"
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

export default ConfirmDialog;
