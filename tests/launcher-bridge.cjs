// Read-only CLI smoke test. Force network offline; never invokes install.
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const root = fs.mkdtempSync(path.resolve('target/launcher-bridge-'));
const exe = path.join(root, 'codex-launcher.exe');
fs.copyFileSync(process.argv[2], exe);
const config = JSON.stringify({ install_mode: 'portable', current_version: '9999.0.0.0' });
fs.writeFileSync(path.join(root, 'updater.json'), config);
fs.writeFileSync(path.join(root, 'codex-launcher.new.exe'), 'must not be cleaned');
const output = execFileSync(exe, ['--bridge-check'], {
  encoding: 'utf8', timeout: 30000, windowsHide: true,
  env: { ...process.env, HTTPS_PROXY: 'http://127.0.0.1:1', HTTP_PROXY: 'http://127.0.0.1:1', ALL_PROXY: 'http://127.0.0.1:1', NO_PROXY: '' },
});
const result = JSON.parse(output);
assert.equal(result.protocol, 1);
assert.equal(result.available, false);
assert.equal(typeof result.error, 'string');
assert.equal(fs.readFileSync(path.join(root, 'updater.json'), 'utf8'), config);
assert.equal(fs.readFileSync(path.join(root, 'codex-launcher.new.exe'), 'utf8'), 'must not be cleaned');
assert.deepEqual(fs.readdirSync(root).sort(), ['codex-launcher.exe', 'codex-launcher.new.exe', 'updater.json']);
const disabled = JSON.stringify({ ...JSON.parse(config), native_updater_bridge: false });
fs.writeFileSync(path.join(root, 'updater.json'), disabled);
const disabledResult = JSON.parse(execFileSync(exe, ['--bridge-check'], {
  encoding: 'utf8', timeout: 5000, windowsHide: true,
}));
assert.deepEqual(disabledResult, { protocol: 1, available: false, error: null });
assert.equal(fs.readFileSync(path.join(root, 'updater.json'), 'utf8'), disabled);
console.log('Release launcher returned bridge JSON without UI, config changes, or cleanup.');
