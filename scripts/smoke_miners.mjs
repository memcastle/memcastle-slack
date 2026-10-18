// Offline end-to-end check: two miners share one named Slack configuration,
// acquire distinct conversations and file into independent wings.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '..');
const binary = process.env.MEMCASTLE_BIN || 'memcastle';
const archive = readdirSync(path.join(root, 'dist')).filter(name => name.endsWith('.tar.gz'));
assert.equal(archive.length, 1, 'build exactly one plugin archive before the smoke check');
const temp = mkdtempSync(path.join(os.tmpdir(), 'memcastle-slack-'));
const palace = path.join(temp, 'palace');
const config = path.join(temp, 'config.toml');
const env = {
  ...process.env,
  PATH: `${path.join(root, 'sources/slack/fixtures/bin')}:${process.env.PATH}`,
  MEMCASTLE_SLACK_BOT_TOKEN: 'fixture-token',
  XDG_STATE_HOME: path.join(temp, 'state'),
};
writeFileSync(config, `
[palace]
path = ${JSON.stringify(palace)}
[server]
port = 0

[[source_configs]]
id = "slack-work"
source = "slack"
credential = { type = "env", name = "MEMCASTLE_SLACK_BOT_TOKEN" }
[source_configs.options]
workspace = "T123456789"
account = "work"
auth_mode = "bot"

[[miners]]
name = "project"
source = "slack"
source_config = "slack-work"
wing = "project"
room = "history"
enabled = false
[miners.options]
conversations = ["C123456789"]

[[miners]]
name = "private"
source = "slack"
source_config = "slack-work"
wing = "private"
room = "history"
enabled = false
[miners.options]
conversations = ["D123456789"]
`);

function cli(...args) {
  const result = spawnSync(binary, ['--config', config, '--palace', palace, '--json', ...args], {
    cwd: root, env, encoding: 'utf8', timeout: 30_000,
  });
  if (result.status !== 0) {
    throw new Error(`${args.join(' ')}: ${result.stderr || result.stdout || result.error}`);
  }
  return result.stdout.trim();
}

function sleep(ms) { return new Promise(resolve => setTimeout(resolve, ms)); }

let logs = '';
const daemon = spawn(binary, ['--config', config, '--palace', palace, 'serve'], {
  cwd: root, env, stdio: ['ignore', 'ignore', 'pipe'],
});
daemon.stderr.on('data', chunk => { logs = `${logs}${chunk}`.slice(-4_000); });

try {
  let ready = false;
  for (let i = 0; i < 50; i++) {
    if (daemon.exitCode !== null) throw new Error(`daemon exited: ${logs}`);
    await sleep(200);
    const status = spawnSync(binary, ['--config', config, '--palace', palace, 'status'], {
      cwd: root, env, encoding: 'utf8', timeout: 5_000,
    });
    if (status.status === 0) { ready = true; break; }
  }
  assert(ready, `daemon did not become ready: ${logs}`);
  cli('plugin', 'install', path.join(root, 'dist', archive[0]), '--yes');
  cli('source', 'enable', 'slack');
  cli('miner', 'enable', 'project');
  cli('miner', 'enable', 'private');
  cli('miner', 'run', 'project');
  cli('miner', 'run', 'private');
  let found = false;
  for (let i = 0; i < 50; i++) {
    await sleep(300);
    const work = cli('search', 'release', '--wing', 'project');
    const personal = cli('search', 'decision', '--wing', 'private');
    if (work.includes('Plan the release') && personal.includes('Private decision')) {
      assert(!cli('search', 'decision', '--wing', 'project').includes('Private decision'));
      assert(!cli('search', 'release', '--wing', 'private').includes('Plan the release'));
      found = true;
      break;
    }
  }
  assert(found, `both independently routed miners must file their selected threads: ${logs}`);
  console.log('PASS two miners, one source configuration, independent scopes and wings');
} finally {
  if (daemon.exitCode === null && daemon.signalCode === null) {
    daemon.kill('SIGTERM');
    await new Promise(resolve => daemon.once('exit', resolve));
  }
  rmSync(temp, { recursive: true, force: true });
}
