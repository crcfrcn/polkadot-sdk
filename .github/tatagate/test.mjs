// 正常、失败及边界测试验证同一公开SDK门禁，不依赖私有资料或其它产品。
import assert from 'node:assert/strict';
import test from 'node:test';
import { gateContract, gatePath, gateRange, gateEnvironment } from './index.mjs';
import { readFileSync, mkdtempSync, mkdirSync, writeFileSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
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

test('完整门禁使用同一Rust对象和独立离线环境，拒绝跳过及工具替换', context => {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), 'sdk-gate-environment-')));
  context.after(() => rmSync(directory, { recursive: true, force: true }));
  const root = join(directory, 'repository'), work = join(directory, 'gate');
  const home = join(directory, 'cargo/home'), packages = join(directory, 'cargo/packages');
  const bin = join(directory, 'rust/bin');
  for (const path of [root, work, home, packages, bin]) mkdirSync(path, { recursive: true });
  for (const tool of ['cargo', 'rustc']) writeFileSync(join(bin, tool), 'fixture', { mode: 0o755 });
  const config = '[net]\noffline = true\n\n[source.crates-io]\nreplace-with = "verified"\n\n[source.verified]\ndirectory = '
    + JSON.stringify(packages) + '\n';
  writeFileSync(join(home, 'config.toml'), config);
  const input = { TATAGATE_CARGO: join(bin, 'cargo'), TATAGATE_RUSTC: join(bin, 'rustc'),
    CARGO: join(bin, 'cargo'), RUSTC: join(bin, 'rustc'), CARGO_HOME: home };
  const execute = tool => tool.endsWith('/cargo') ? 'cargo 1.97.1 fixture' : 'rustc 1.97.1 fixture';
  const result = gateEnvironment(root, work, input, { execute });
  assert.equal(result.CARGO_NET_OFFLINE, 'true'); assert.equal(result.CARGO_TARGET_DIR, join(work, 'cargo-target'));
  assert.equal(result.RUSTC, input.RUSTC); assert.equal(result.WASM_BUILD_STD, '1');
  assert.equal(result.WASM_BUILD_WORKSPACE_HINT, root);
  assert.equal(gateEnvironment(root, work, { ...input, WASM_BUILD_WORKSPACE_HINT: '/other/repository' },
    { execute }).WASM_BUILD_WORKSPACE_HINT, root);
  for (const key of ['SKIP_PALLET_REVIVE_FIXTURES', 'SKIP_WASM_BUILD', 'RUSTC_WRAPPER', 'RUSTUP_TOOLCHAIN']) {
    assert.throws(() => gateEnvironment(root, work, { ...input, [key]: '0' }, { execute }), /拒绝/u);
  }
  for (const changes of [{ RUSTC: '/other/rustc' }, { CARGO: '/other/cargo' }, { CARGO_HOME: root }]) {
    assert.throws(() => gateEnvironment(root, work, { ...input, ...changes }, { execute }));
  }
  assert.throws(() => gateEnvironment(root, root, input, { execute }), /中间物/u);
  assert.throws(() => gateEnvironment(root, work, input, { execute: () => 'rustc 1.97.0 fixture' }), /版本/u);
  symlinkSync(home, join(directory, 'home-link'));
  assert.throws(() => gateEnvironment(root, work, { ...input, CARGO_HOME: join(directory, 'home-link') }, { execute }));
  writeFileSync(join(home, 'config.toml'), config.replace('offline = true', 'offline = false'));
  assert.throws(() => gateEnvironment(root, work, input, { execute }), /目录源/u);
});
