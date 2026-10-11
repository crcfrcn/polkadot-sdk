// 本产品配置需求的公开入口；只登记名称、类型和规则，不保存配置值。
export const manifest=Object.freeze({
  "schema": 1,
  "product_id": "polkadot-sdk",
  "configuration": {
    "id": "polkadot-sdk",
    "kind": "sdk",
    "title": "Polkadot SDK配置",
    "items": []
  }
});
export function describeConfiguration(){return JSON.parse(JSON.stringify(manifest));}
