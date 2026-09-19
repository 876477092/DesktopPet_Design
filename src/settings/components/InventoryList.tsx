import React from 'react';

/**
 * 背包清单（S8-M6；`02 §5.18`）。
 *
 * 数据来自后端 `pet://state` 的 inventory 段（`[{itemId, qty}]`）。
 * 本组件纯展示：按 itemId 聚合计数，空背包给占位文案。
 */
export interface InventoryItemView {
  itemId: string;
  qty: number;
}

export interface InventoryListProps {
  items: InventoryItemView[];
}

export function InventoryList({ items }: InventoryListProps): React.ReactElement {
  return (
    <section className="dp-inventory" data-testid="inventory-list">
      <h4 className="dp-inventory-title">背包</h4>
      {items.length === 0 ? (
        <p className="dp-inventory-empty">背包空空如也，去商城看看吧～</p>
      ) : (
        <ul className="dp-inventory-list">
          {items.map((it) => (
            <li key={it.itemId} className="dp-inventory-item" data-item-id={it.itemId}>
              <span className="dp-inventory-item-name">{it.itemId}</span>
              <span className="dp-inventory-item-qty">×{it.qty}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export default InventoryList;
