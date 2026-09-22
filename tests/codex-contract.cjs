// Exercise the inspected app's actual updater classes without starting Codex.
// Arguments: app.asar sidecar.dll compiled bridge-helper.exe
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { setTimeout: delay } = require('node:timers/promises');

async function main() {
  const fd = fs.openSync(process.argv[2], 'r');
  let source;
  try {
    const header = Buffer.alloc(16);
    fs.readSync(fd, header, 0, 16, 0);
    const data = Buffer.alloc(header.readUInt32LE(12));
    fs.readSync(fd, data, 0, data.length, 16);
    const files = JSON.parse(data).files['.vite'].files.build.files;
    const name = Object.keys(files).find(n => n.startsWith('bootstrap-') && n.endsWith('.js'));
    const entry = files[name];
    const script = Buffer.alloc(entry.size);
    fs.readSync(fd, script, 0, script.length, 8 + header.readUInt32LE(4) + Number(entry.offset));
    source = script.toString();
  } finally { fs.closeSync(fd); }
  function section(start, end) {
    const i = source.indexOf(start);
    const j = source.indexOf(end, i + start.length);
    assert(i >= 0 && j > i, `Unrecognized updater contract: ${start}`);
    return source.slice(i, j);
  }
  const root = fs.mkdtempSync(path.resolve('target/codex-contract-'));
  const addonPath = path.join(root, 'windows-updater.node');
  const launcher = path.join(root, 'launcher.exe');
  fs.copyFileSync(process.argv[3], addonPath);
  fs.copyFileSync(process.argv[4], launcher);
  fs.writeFileSync(path.join(root, 'updater.json'), '{}');
  process.env.CODEX_UPDATER_LAUNCHER = launcher;
  process.env.BRIDGE_TEST_INSTALL_LOG = path.join(root, 'install.log');
  const addon = require(addonPath);
  let calls = 0, ready = false, prepared = false, installed = false;
  const nativeAddon = { ...addon, trySilentDownloadStoreUpdates: cb => {
    ++calls; return addon.trySilentDownloadStoreUpdates(cb);
  } };
  let manifestVersion = '26.916.1.0';
  const context = vm.createContext({
    o: { app: { isPackaged: true, getName: () => 'Codex' }, dialog: { showMessageBox: async () => ({ response: 0 }) } },
    MT: () => 'https://example.invalid/manifest',
    NT: async () => ({ buildVersion: manifestVersion, storeProductId: 'test', packageIdentity: 'OpenAI.Codex' }),
    bT: value => /^\d+(\.\d+){3}$/.test(value),
    yT: (a, b) => a.localeCompare(b, undefined, { numeric: true }),
    AT: 1800000, CT: async () => {},
    FT: () => ({ warning() {}, info() {}, error() {} }),
    setInterval, Error, Date,
  });
  vm.runInContext(section('function PT(', 'var FT='), context);
  const StoreUpdater = vm.runInContext(`(${section('IT=class', 'LT=class').slice(3).replace(/,$/, '')})`, context);
  const FallbackUpdater = vm.runInContext(`(${section('LT=class', 'RT=').slice(3).replace(/,$/, '')})`, context);
  const store = new StoreUpdater({
    nativeAddon, storeProductId: 'test', packageIdentity: 'OpenAI.Codex',
    storeUpdateManifestUrl: 'https://example.invalid/manifest', buildVersion: '26.915.1.0',
    checkIntervalMs: 0, checkOnInitialize: false,
    onUpdateReadyChanged: value => { ready = value; },
    onBeforeInstall: async () => { prepared = true; },
    onInstallUpdatesRequested: () => { installed = true; },
  });
  await store.initialize();
  assert.equal(store.hasUpdater(), true);
  manifestVersion = '26.914.1.0';
  await store.checkForUpdates();
  assert.equal(calls, 0, 'Codex manifest gate must remain intact');
  manifestVersion = '26.916.1.0';
  process.env.BRIDGE_TEST_REPLY = JSON.stringify({ protocol: 1, available: false });
  await store.checkForUpdates();
  assert.equal(calls, 1); assert.equal(ready, false);
  process.env.BRIDGE_TEST_REPLY = JSON.stringify({ protocol: 1, available: true });
  await store.checkForUpdates();
  assert.equal(calls, 2); assert.equal(ready, true);
  const wrapper = new FallbackUpdater({
    storeUpdater: store, checkIntervalMs: 0,
    msixFallbackUpdater: { initialize: async () => addon.armProcessTreeCleanup() },
  });
  await wrapper.initialize();
  assert.equal(wrapper.hasUpdater(), true);
  await wrapper.installUpdatesIfAvailable();
  assert.equal(prepared, true); assert.equal(installed, true); assert.equal(ready, false);
  for (let i = 0; !fs.existsSync(process.env.BRIDGE_TEST_INSTALL_LOG) && i < 100; ++i) await delay(50);
  assert.equal(fs.readFileSync(process.env.BRIDGE_TEST_INSTALL_LOG, 'utf8'), '--bridge-install');
  console.log('Actual Codex Store/wrapper classes passed manifest gate, readiness, fallback, and install handoff tests.');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
