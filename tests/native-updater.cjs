// Usage: node tests/native-updater.cjs <sidecar.dll> <compiled bridge-helper.exe>
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { setTimeout: delay } = require('node:timers/promises');

async function main() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'codex-bridge-'));
  try {
    const addonPath = path.join(root, 'windows-updater.node');
    const launcher = path.join(root, 'codex-launcher.exe');
    fs.copyFileSync(process.argv[2], addonPath);
    fs.copyFileSync(process.argv[3], launcher);
    fs.writeFileSync(path.join(root, 'updater.json'), '{}');
    const addon = require(addonPath);
    assert.equal(addon.getCurrentPackageFamily(), 'OpenAI.Codex_2p2nqsd0c76g0');
    assert.throws(() => addon.armProcessTreeCleanup(), /managed by codex-launcher/);
    assert.throws(() => addon.stagePackage('ignored.msix'), /managed by codex-launcher/);
    delete process.env.CODEX_UPDATER_LAUNCHER;
    await assert.rejects(addon.trySilentDownloadStoreUpdates(() => {}), /started through/);
    process.env.CODEX_UPDATER_LAUNCHER = launcher;
    for (const available of [false, true]) {
      process.env.BRIDGE_TEST_REPLY = JSON.stringify({ protocol: 1, available, error: null });
      const result = await addon.trySilentDownloadStoreUpdates(() => {});
      assert.deepEqual(result, {
        hasUpdate: available, canSilentlyDownload: true, completed: true, overallState: 'Completed',
      });
    }
    process.env.BRIDGE_TEST_REPLY = JSON.stringify({ protocol: 1, available: false, error: 'offline' });
    await assert.rejects(addon.trySilentDownloadStoreUpdates(() => {}), /offline/);
    process.env.BRIDGE_TEST_REPLY = 'not JSON';
    await assert.rejects(addon.trySilentDownloadStoreUpdates(() => {}), /Invalid launcher reply/);
    process.env.BRIDGE_TEST_REPLY = JSON.stringify({ protocol: 2, available: true });
    await assert.rejects(addon.trySilentDownloadStoreUpdates(() => {}), /Unsupported/);
    const log = path.join(root, 'install.log');
    process.env.BRIDGE_TEST_INSTALL_LOG = log;
    const result = await addon.trySilentDownloadAndInstallStoreUpdates(() => {});
    assert.equal(result.completed, true);
    for (let i = 0; !fs.existsSync(log) && i < 100; ++i) await delay(50);
    assert.equal(fs.readFileSync(log, 'utf8'), '--bridge-install');
    console.log('Native updater contract checks passed (mock launcher; no app terminated).');
  } finally {
    // Windows keeps the loaded .node mapped until this Node process exits.
    for (const name of fs.readdirSync(root)) {
      if (name.endsWith('.node')) continue;
      // The helper writes its receipt just before exiting; wait for Windows
      // to release its executable image rather than racing that final exit.
      for (let attempt = 0; ; ++attempt) {
        try { fs.rmSync(path.join(root, name), { force: true }); break; }
        catch (error) { if (attempt === 40) throw error; await delay(50); }
      }
    }
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
