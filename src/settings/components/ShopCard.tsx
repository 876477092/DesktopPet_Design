import React, { useCallback, useState } from 'react';

import { invokeCommand } from '../../shared/ipc';

/**
 * 商城卡片（S8-M6；`01 §8.5` 商城 / `02 §5.18`）。
 *
 * 数据：商品目录（30 件）来自 `shop.json`，由后端 `pet://state` 的 economy 段推送；
 * 本组件只负责渲染与购买按钮。购买经 `invoke('pet_buy', { itemId, qty })` 投递，
 * 校验（余额 / 限购 / 失败冲正）在 core-loop 完成，结果由 economy 段回传。
 *
 * 入参为目录快照（由页面注入），组件不自行取数，保持可测。
 */
export interface ShopItemView {
  id: string;
  name: string;
  price: number;
  category: string;
  locked?: boolean;
}

export interface ShopCardProps {
  /** 商品目录（后端 shop.json 的可见子集）。 */
  items: ShopItemView[];
  /** 当前心币余额（灰化不可购买项用）。 */
  coin: number;
}

export function ShopCard({ items, coin }: ShopCardProps): React.ReactElement {
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const onBuy = useCallback(
    (item: ShopItemView) => {
      if (item.locked) return;
      setPending(item.id);
      setError(null);
      void invokeCommand<void>('pet_buy', { itemId: item.id, qty: 1 })
        .catch((err: unknown) => {
          setError(err instanceof Error ? err.message : String(err));
        })
        .finally(() => setPending(null));
    },
    [],
  );

  return (
    <section className="dp-shop" data-testid="shop-card">
      <header className="dp-shop-head">
        <span className="dp-shop-coin" data-testid="shop-coin">
          {coin} 心币
        </span>
      </header>
      <ul className="dp-shop-list">
        {items.map((item) => {
          const afford = coin >= item.price;
          const disabled = item.locked || !afford || pending === item.id;
          return (
            <li
              key={item.id}
              className={`dp-shop-item${item.locked ? ' dp-locked' : ''}`}
              data-item-id={item.id}
            >
              <span className="dp-shop-item-name">{item.name}</span>
              <span className="dp-shop-item-price">{item.price}</span>
              <button
                type="button"
                className="dp-shop-buy"
                disabled={disabled}
                onClick={() => onBuy(item)}
              >
                {item.locked ? '未解锁' : '购买'}
              </button>
            </li>
          );
        })}
      </ul>
      {error ? <p className="dp-shop-error">{error}</p> : null}
    </section>
  );
}

export default ShopCard;
