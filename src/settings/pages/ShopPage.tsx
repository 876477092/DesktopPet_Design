import React from 'react';

import { usePetSnapshot } from '../hooks/usePetSnapshot';
import { useTranslator } from '../store/useSettings';
import ShopCard from '../components/ShopCard';
import InventoryList from '../components/InventoryList';
import { SHOP_CATALOG, findShopItem, type ShopCatalogItem } from '@shared/shopCatalog';

/**
 * 商城 + 背包 Tab（`01 §8.5`；FR-14 商城 / 背包；S10-M1）。
 *
 * 数据全来自快照 / 静态配置，UI 不硬编码：
 * - 商品目录：`shop.json`（`SHOP_CATALOG`）；
 * - 余额：`pet://state.economy.coin`（`ShopCard` 据此判可买）；
 * - 背包：`pet://state.inventory`（`InventoryList` 渲染；名从目录反查）。
 *
 * 购买走 `pet_buy`（`ShopCard` 内部），余额 / 限额 / 失败冲正权威在 Rust。
 */
export function ShopPage(): React.ReactElement {
  const t = useTranslator();
  const snap = usePetSnapshot();

  const coin = snap?.economy.coin ?? 0;
  const catalog: ShopCatalogItem[] = [...SHOP_CATALOG];

  const inventory = (snap?.inventory ?? []).map((it) => ({
    itemId: it.itemId,
    name: findShopItem(it.itemId)?.name ?? it.itemId,
    qty: it.qty,
  }));

  return (
    <section className="dp-page" aria-label={t('app.tab.shop')}>
      <div className="dp-shop-balance" data-testid="shop-balance">
        {t('shop.balance', { coin: String(coin) })}
      </div>
      <ShopCard items={catalog} coin={coin} />

      <h4 className="dp-section-title">{t('shop.inventory.section')}</h4>
      <InventoryList items={inventory} />
    </section>
  );
}

export default ShopPage;
