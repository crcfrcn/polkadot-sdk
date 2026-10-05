// SDK独立门禁：准确提交、上游来源、自有改动和适用测试均失败关闭。
import { execFileSync, spawnSync } from 'node:child_process';
import { lstatSync, readFileSync, realpathSync, existsSync } from 'node:fs';
import { resolve, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const shaPattern = /^[0-9a-f]{40}$/u;
export function gateRange(base, head) {
  if (!shaPattern.test(base ?? '') || !shaPattern.test(head ?? '') || base === head) throw new Error('SDK门禁提交区间无效');
  return { base, head };
}
export function gateContract(value) {
  if (value?.schema !== 1 || value.repository !== 'polkadot-sdk' || value.upstream !== 'paritytech/polkadot-sdk'
    || !shaPattern.test(value.upstream_base ?? '') || !shaPattern.test(value.initial_sha ?? '')
    || !Array.isArray(value.checks) || value.checks.join(',') !== 'identity,history,source,workflow,tests,changed-crates') {
    throw new Error('SDK门禁合同无效');
  }
  return value;
}
export function gatePath(value) {
  if (!value || value.startsWith('/') || value.includes('\\') || value.split('/').some(part => ['', '.', '..'].includes(part))) {
    throw new Error('SDK门禁路径越界');
  }
  return value;
}
function command(file, args, root, environment = process.env) {
  if (!file || !file.startsWith('/') || realpathSync(file) !== file
    || !lstatSync(file).isFile() || !(lstatSync(file).mode & 0o111)) throw new Error('SDK门禁缺少准确工具路径');
  const result = spawnSync(file, args, { cwd: root, env: environment, encoding: 'utf8', timeout: 3_600_000,
    // SDK完整metadata当前约13MiB，保留有界缓冲以容纳完整依赖图。
    stdio: ['ignore', 'pipe', 'pipe'], maxBuffer: 32 * 1024 * 1024 });
  if (result.status !== 0 || result.error) {
    process.stderr.write((result.stderr ?? '').slice(-64 * 1024));
    throw new Error('SDK门禁工具检查失败：' + args.slice(0, 4).join(' ') + '；退出码 ' + result.status
      + (result.error ? '；' + result.error.message : ''));
  }
  return result.stdout.trim();
}

// 门禁只使用调用方已准备的离线目录源；完整夹具和Runtime构建不能跳过。
export function gateEnvironment(root, work, input, { canonical = realpathSync, stat = lstatSync, execute = command } = {}) {
  for (const key of ['SKIP_PALLET_REVIVE_FIXTURES', 'SKIP_WASM_BUILD', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
    'RUSTUP_TOOLCHAIN', 'WASM_BUILD_TOOLCHAIN', 'PALLET_REVIVE_FIXTURES_RUSTUP_TOOLCHAIN']) {
    if (Object.hasOwn(input, key)) throw new Error('SDK完整门禁拒绝跳过或替换工具：' + key);
  }
  const regular = path => typeof path === 'string' && path.startsWith('/') && resolve(path) === path
    && canonical(path) === path && stat(path).isFile() && !!(stat(path).mode & 0o111);
  const cargo = input.TATAGATE_CARGO, rustc = input.TATAGATE_RUSTC;
  if (!regular(cargo) || !regular(rustc) || dirname(cargo) !== dirname(rustc)
    || input.RUSTC !== rustc || input.CARGO !== cargo
    || execute(rustc, ['--version'], root, input).split(' ')[1] !== '1.97.1'
    || execute(cargo, ['--version'], root, input).split(' ')[1] !== '1.97.1') throw new Error('SDK门禁Rust对象或版本不符');
  if (!work || !work.startsWith('/') || resolve(work) !== work || work === root || work.startsWith(root + '/')
    || canonical(work) !== work || !stat(work).isDirectory()) throw new Error('SDK检查中间物目录无效');
  const home = input.CARGO_HOME;
  const boundary = dirname(root) === dirname(work) ? dirname(work) : work;
  if (typeof home !== 'string' || !home.startsWith(boundary + '/') || home.startsWith(root + '/')
    || canonical(home) !== home || !stat(home).isDirectory()) throw new Error('SDK门禁缺少独立离线Cargo主目录');
  const config = join(home, 'config.toml');
  if (canonical(config) !== config || !stat(config).isFile()) throw new Error('SDK门禁离线配置不是准确文件');
  const text = readFileSync(config, 'utf8');
  const directory = text.match(/^directory = (".*")$/mu)?.[1];
  const packages = directory && JSON.parse(directory);
  if (!/^\[net\]\noffline = true\n\n\[source\.crates-io\]\nreplace-with = "verified"\n\n\[source\.verified\]\ndirectory = "[^\n]+"\n$/u.test(text)
    || typeof packages !== 'string' || !packages.startsWith(boundary + '/') || packages.startsWith(root + '/')
    || canonical(packages) !== packages || !stat(packages).isDirectory()) throw new Error('SDK门禁缺少完整离线目录源');
  return { ...input, RUSTC: rustc, CARGO: cargo, CARGO_NET_OFFLINE: 'true',
    CARGO_TARGET_DIR: join(work, 'cargo-target'), RUSTC_BOOTSTRAP: '1', WASM_BUILD_STD: '1',
    // OUT_DIR位于独立临时目录，WASM子工程必须明确继承受检源码的原锁。
    WASM_BUILD_WORKSPACE_HINT: root };
}
function git(root, args) {
  const tool = process.env.PRODUCT_GIT_BIN;
  if (!tool || realpathSync(tool) !== tool || !lstatSync(tool).isFile()) throw new Error('SDK门禁缺少准确Git');
  if (command(tool, ['--version'], root) !== 'git version 2.54.0') throw new Error('SDK门禁Git版本不符');
  return execFileSync(tool, ['-C', root, ...args], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024,
    env: { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null' }, stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}
function identity(root, physical = false) {
  if (realpathSync(root) !== root || !lstatSync(root + '/.git').isDirectory()
    || git(root, ['rev-parse', '--show-toplevel']) !== root
    || git(root, ['remote', 'get-url', 'origin']) !== 'https://github.com/crcfrcn/polkadot-sdk.git') throw new Error('SDK门禁仓库身份无效');
  if (physical && (root !== '/Users/rhett/polkadot-sdk' || git(root, ['branch', '--show-current']) !== 'main'
    || git(root, ['remote', 'get-url', 'upstream']) !== 'https://github.com/paritytech/polkadot-sdk.git')) throw new Error('SDK正式开发入口无效');
}
function checkedSource(root, path) {
  const absolute = resolve(root, gatePath(path));
  if (!absolute.startsWith(root + '/') || realpathSync(absolute) !== absolute || !lstatSync(absolute).isFile()) throw new Error('SDK源码不是准确文件');
  const text = readFileSync(absolute, 'utf8');
  if (/-----BEGIN [A-Z ]*PRIVATE KEY-----|github_pat_[A-Za-z0-9_]{20,}|ghs_[A-Za-z0-9_]{20,}/u.test(text)
    || /\/Users\/[A-Za-z0-9_-]+\/(?:tataconsole|GMB|TATA|TUYU)\//u.test(text)) throw new Error('SDK源码包含机密或私有工作区路径');
}
export function runGate(root, baseSHA, headSHA, work) {
  root = resolve(root); identity(root); const range = gateRange(baseSHA, headSHA);
  const contract = gateContract(JSON.parse(readFileSync(join(root, '.github/tatagate/contracts.json'), 'utf8')));
  if (git(root, ['rev-parse', 'HEAD']) !== range.head || git(root, ['status', '--porcelain=v1', '--untracked-files=all']) !== '') throw new Error('SDK门禁快照不一致');
  git(root, ['merge-base', '--is-ancestor', range.base, range.head]);
  git(root, ['merge-base', '--is-ancestor', contract.initial_sha, range.head]);
  git(root, ['merge-base', '--is-ancestor', contract.upstream_base, contract.initial_sha]);
  git(root, ['diff', '--check', range.base, range.head]);
  const paths = git(root, ['diff', '--name-only', '-z', range.base, range.head]).split('\u0000').filter(Boolean);
  const crates = new Set();
  for (const path of paths) {
    gatePath(path);
    const absolute = join(root, path);
    if (!existsSync(absolute)) continue;
    checkedSource(root, path);
    if (path.startsWith('.github/workflows/') && /\.ya?ml$/u.test(path)) {
      command(process.env.TATAGATE_ACTIONLINT, ['-shellcheck=', '-pyflakes=', path], root);
    }
    if (path.endsWith('.rs') || path.endsWith('Cargo.toml')) {
      let directory = dirname(absolute);
      while (directory !== root && directory.startsWith(root + '/')) {
        const manifest = join(directory, 'Cargo.toml');
        if (existsSync(manifest)) {
          const source = readFileSync(manifest, 'utf8');
          const packageBlock = source.match(/\[package\]([\s\S]*?)(?=\n\[|$)/u)?.[1];
          const name = packageBlock?.match(/^name\s*=\s*"([a-z0-9-]+)"/mu)?.[1];
          if (name) { crates.add(name); break; }
        }
        directory = dirname(directory);
      }
    }
  }
  command(process.execPath, ['--test', '.github/tatagate/test.mjs'], root);
  if (crates.size || paths.includes('Cargo.lock') || paths.includes('Cargo.toml')) {
    const environment = gateEnvironment(root, work, process.env), cargo = environment.CARGO;
    command(cargo, ['metadata', '--locked', '--offline', '--format-version', '1'], root, environment);
    for (const name of crates) {
      command(cargo, ['check', '--locked', '--offline', '-p', name], root, environment);
      command(cargo, ['test', '--locked', '--offline', '-p', name, '--lib'], root, environment);
    }
    // workspace声明与锁整体改动需要全工作空间检查，不能只验证门禁自身。
    if (paths.includes('Cargo.lock') || paths.includes('Cargo.toml')) command(cargo, ['check', '--locked', '--offline', '--workspace'], root, environment);
  }
  return { repository: 'polkadot-sdk', base_sha: range.base, head_sha: range.head, changed_crates: [...crates].sort() };
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [mode, ...args] = process.argv.slice(2);
    if (mode === 'physical' && args.length === 1) identity(resolve(args[0]), true);
    else if (mode === 'local' && args.length === 4) console.log(JSON.stringify(runGate(args[0], args[1], args[2], args[3])));
    else if (mode === 'remote' && args.length === 0) {
      const event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
      if (process.env.GITHUB_REPOSITORY !== 'crcfrcn/polkadot-sdk' || event.ref !== 'refs/heads/main'
        || event.repository?.full_name !== 'crcfrcn/polkadot-sdk' || event.after !== process.env.GITHUB_SHA) throw new Error('SDK远端门禁事件身份不符');
      console.log(JSON.stringify(runGate(process.cwd(), event.before, event.after, process.env.RUNNER_TEMP)));
    } else throw new Error('SDK门禁参数无效');
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
