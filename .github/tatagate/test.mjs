// 正常、失败及边界测试验证同一公开SDK门禁，不依赖私有资料或其它产品。
import assert from 'node:assert/strict';
import test from 'node:test';
import { gateContract, gatePath, gateRange } from './index.mjs';
import { readFileSync } from 'node:fs';
const contract = JSON.parse(readFileSync(new URL('./contracts.json', import.meta.url), 'utf8'));
test('准确SDK来源与提交区间有效', () => {
  assert.equal(gateContract(contract).repository, 'polkadot-sdk');
  assert.deepEqual(gateRange('a'.repeat(40), 'b'.repeat(40)), { base: 'a'.repeat(40), head: 'b'.repeat(40) });
  assert.equal(gatePath('substrate/frame/revive/src/lib.rs'), 'substrate/frame/revive/src/lib.rs');
});
test('错仓、错来源、缺检查与非法区间拒绝', () => {
  for (const change of [{ repository: 'citizenchain' }, { upstream: 'other/polkadot-sdk' }, { checks: [] }, { upstream_base: 'main' }, { schema: 2 }]) {
    assert.throws(() => gateContract({ ...contract, ...change }));
  }
  for (const pair of [['a', 'b'], ['a'.repeat(40), 'a'.repeat(40)], [null, 'b'.repeat(40)], ['A'.repeat(40), 'b'.repeat(40)]]) {
    assert.throws(() => gateRange(...pair));
  }
});
test('绝对路径、空段及路径穿越拒绝', () => {
  for (const path of ['', '/substrate/lib.rs', '../lib.rs', 'a/../b', 'a//b', 'a\\b']) assert.throws(() => gatePath(path));
});
