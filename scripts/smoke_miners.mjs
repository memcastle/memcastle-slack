// Offline installed-archive lifecycle checks, including identical-destination miners.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '..');
const binary = process.env.MEMCASTLE_BIN || 'memcastle';
const archive = readdirSync(path.join(root, 'dist')).filter(name => name.endsWith('.tar.gz'));
assert.equal(archive.length, 1, 'build exactly one plugin archive before the smoke check');
const temp = mkdtempSync(path.join(os.tmpdir(), 'memcastle-slack-'));
const palace = path.join(temp, 'palace');
const config = path.join(temp, 'config.toml');
const personalToken = path.join(temp, 'personal-token');
const fixtureBin = path.join(temp, 'bin');
mkdirSync(fixtureBin);
copyFileSync(path.join(root, 'sources/slack/fixtures/bin/curl'), path.join(fixtureBin, 'curl'));
function scenario(name) {
  const file = path.join(fixtureBin, 'fixture-state.json');
  writeFileSync(`${file}.tmp`, JSON.stringify({ scenario: name }));
  renameSync(`${file}.tmp`, file);
}
writeFileSync(personalToken, 'fixture-personal\n', { mode: 0o600 });
const env = {
  ...process.env,
  PATH: `${fixtureBin}:${process.env.PATH}`,
  MEMCASTLE_SLACK_WORK_TOKEN: 'fixture-work',
  MEMCASTLE_SLACK_OTHER_BOT_TOKEN: 'fixture-other-bot',
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
credential = { type = "env", name = "MEMCASTLE_SLACK_WORK_TOKEN" }
[source_configs.options]
workspace = "T123456789"
account = "work"
auth_mode = "bot"

[[source_configs]]
id = "slack-personal"
source = "slack"
credential = { type = "file", path = ${JSON.stringify(personalToken)} }
[source_configs.options]
workspace = "T987654321"
account = "personal-user"
auth_mode = "user"

[[source_configs]]
id = "slack-other-bot"
source = "slack"
credential = { type = "env", name = "MEMCASTLE_SLACK_OTHER_BOT_TOKEN" }
[source_configs.options]
workspace = "T123456789"
account = "other-bot"
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
name = "project-twin"
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

[[miners]]
name = "personal"
source = "slack"
source_config = "slack-personal"
wing = "personal"
room = "history"
enabled = false
[miners.options]
conversations = ["D123456789"]

[[miners]]
name = "other-bot"
source = "slack"
source_config = "slack-other-bot"
wing = "other-bot"
room = "history"
enabled = false
[miners.options]
conversations = ["G123456789"]
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

async function run(name, { full = false, fail = false } = {}) {
  const submitted = JSON.parse(cli('miner', 'run', name, ...(full ? ['--full'] : [])));
  assert(submitted.id, `missing submitted job ID: ${JSON.stringify(submitted)}`);
  for (let i = 0; i < 200; i++) {
    const job = JSON.parse(cli('job', 'show', submitted.id));
    if (['completed', 'failed', 'cancelled'].includes(job.status)) {
      assert.equal(job.status, fail ? 'failed' : 'completed', JSON.stringify(job));
      return job;
    }
    await sleep(100);
  }
  throw new Error(`job ${submitted.id} did not terminate`);
}

function drawers(sourceId) {
  return JSON.parse(cli('drawer', 'list', '--room', 'project/history', '--limit', '200'))
    .filter(drawer => drawer.source.origin.source_id === sourceId)
    .map(drawer => JSON.parse(cli('drawer', 'show', `project/history/${drawer.id}`)))
    .filter(drawer => !drawer.valid_to);
}

function content(sourceId) { return drawers(sourceId).map(drawer => drawer.content).join('\n'); }

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
  cli('miner', 'enable', 'project-twin');
  cli('miner', 'enable', 'private');
  cli('miner', 'enable', 'personal');
  cli('miner', 'enable', 'other-bot');
  const project = (await run('project')).result.source_id;
  const twin = (await run('project-twin')).result.source_id;
  assert.notEqual(project, twin, 'identical-origin/destination named miners must have independent identities');
  await run('private');
  await run('personal');
  await run('other-bot');
  let found = false;
  for (let i = 0; i < 50; i++) {
    await sleep(300);
    const work = cli('search', 'release', '--wing', 'project');
    const privateDm = cli('search', 'decision', '--wing', 'private');
    const personal = cli('search', 'decision', '--wing', 'personal');
    const otherBot = cli('search', 'status', '--wing', 'other-bot');
    if (work.includes('Plan the release') && privateDm.includes('Private decision')
        && personal.includes('Personal decision') && otherBot.includes('Status update')) {
      assert(!cli('search', 'decision', '--wing', 'project').includes('Private decision'));
      assert(!cli('search', 'release', '--wing', 'private').includes('Plan the release'));
      assert(!personal.includes('Private decision'));
      assert(!privateDm.includes('Personal decision'));
      found = true;
      break;
    }
  }
  assert(found, `shared and independently authenticated miners must file only their selected threads: ${logs}`);
  const originals = drawers(project);
  const originalRevision = originals.find(d => d.source.origin.document.endsWith('000000.000001')).source.origin.revision;
  await run('project');
  assert.deepEqual(drawers(project).map(d => d.id), originals.map(d => d.id), 'unchanged run is idempotent');

  scenario('edit');
  await run('project'); // old known thread must refresh without --full or history rediscovery
  assert(content(project).includes('Ship on Monday'));
  assert(!content(project).includes('Ship on Friday'));
  assert(content(twin).includes('Ship on Friday'));
  const history = JSON.parse(cli('drawer', 'history', `project/history/${originals.find(d => d.content.includes('Ship on Friday')).id}`));
  assert(history.versions.some(d => d.valid_to && d.content.includes('Ship on Friday')), 'edits retain closed historical content');

  scenario('deleted-reply');
  await run('project', { full: true });
  assert(content(project).includes('Plan the release'));
  assert(!content(project).includes('Ship on'));

  for (const name of ['retention', 'empty-thread', 'missing-thread', 'archived']) {
    scenario(name);
    const before = content(project);
    await run('project');
    assert(content(project).includes('Plan the release'), `${name} preserves retained roots`);
    if (name !== 'archived') assert.equal(content(project), before);
  }
  for (const name of ['invalid', 'wrong-workspace', 'missing-scope', 'rate-limit', 'http-forbidden', 'partial', 'truncated']) {
    scenario(name);
    const before = content(project);
    await run('project', { fail: true });
    assert.equal(content(project), before, `${name} must not retire or replace content`);
  }

  scenario('work');
  cli('miner', 'set', 'project', '--option', 'users=["U000000001"]');
  await run('project');
  assert.equal(drawers(project).length, 0, 'narrowing user membership retires selected miner content');
  assert(content(twin).includes('Ship on Friday'), 'retirement never touches twin inventory');
  const retired = JSON.parse(cli('drawer', 'history', `project/history/${originals[0].id}`));
  assert(retired.versions.every(d => d.valid_to), 'retirement retains history');
  cli('miner', 'set', 'project', '--unset-option', 'users', '--allow-broaden');
  await run('project');
  const restored = drawers(project).find(d => d.source.origin.document.endsWith('000000.000001'));
  assert.equal(restored.source.origin.revision, originalRevision, 'retired content restores even at the original unchanged provider revision');

  scenario('paged-replies');
  await run('project');
  assert(content(project).includes('Paged reply 20'), 'acquisition includes the second replies page');
  scenario('paged-replies-edit');
  await run('project');
  assert(content(project).includes('Edited later-page reply'));
  assert(!content(project).includes('Paged reply 20'));
  scenario('paged-replies-deleted');
  await run('project');
  assert(content(project).includes('Paged reply 19'));
  assert(!content(project).includes('Edited later-page reply'));
  scenario('work');
  await run('project');

  cli('miner', 'set', 'project', '--option', 'threads=["C123456789:1760000002.000001"]');
  assert.equal((await run('project')).result.source_id, project);
  assert(!content(project).includes('Plan the release'));
  assert(content(project).includes('Status update'));
  cli('miner', 'set', 'project', '--unset-option', 'threads', '--allow-broaden');
  await run('project');
  cli('miner', 'set', 'project', '--option', 'until="1760000000.000001"', '--allow-broaden');
  await run('project');
  assert(content(project).includes('Plan the release'));
  assert(!content(project).includes('Ship on Friday'));
  assert(!content(project).includes('Status update'));
  cli('miner', 'set', 'project', '--unset-option', 'until', '--allow-broaden');
  await run('project');
  cli('miner', 'set', 'project', '--option', 'conversations=["D123456789"]', '--allow-broaden');
  assert.equal((await run('project')).result.source_id, project);
  assert(!content(project).includes('Plan the release'));
  assert(content(project).includes('Private decision'));
  cli('miner', 'set', 'project', '--option', 'conversations=["C123456789"]', '--allow-broaden');
  await run('project');
  assert(content(project).includes('Ship on Friday'));
  assert(!content(project).includes('Private decision'));

  scenario('lost-access');
  await run('project');
  assert.equal(drawers(project).length, 0);
  assert(content(twin).includes('Ship on Friday'));
  scenario('work');
  await run('project', { full: true });
  assert(content(project).includes('Ship on Friday'));
  scenario('removed');
  await run('project');
  assert(!content(project).includes('Plan the release'));
  assert(content(project).includes('Status update'));
  scenario('work');
  await run('project');

  // A pending inventory entry must recover even though its first candidate cursor never committed.
  cli('miner', 'set', 'pending', '--source', 'slack', '--source-config', 'slack-work', '--wing', 'pending', '--room', 'history', '--option', 'conversations=["C123456789"]', '--max-documents', '1', '--allow-broaden');
  scenario('failed-first');
  await run('pending', { fail: true });
  scenario('work');
  cli('miner', 'set', 'pending', '--option', 'threads=["C123456789:1760000000.000001"]');
  await run('pending'); // retire the pending excluded root, even without acquired metadata
  cli('miner', 'set', 'pending', '--unset-option', 'threads', '--allow-broaden');
  await run('pending');
  await run('pending');
  await run('pending');
  assert(cli('search', 'release', '--wing', 'pending', '--ranking', 'lexical').includes('Plan the release'));
  assert(cli('search', 'status', '--wing', 'pending', '--ranking', 'lexical').includes('Status update'));
  console.log('PASS credentials, same-destination isolation, edits, retirement/history, unchanged-revision restoration, retention, failures and limited pending recovery');
} finally {
  if (daemon.exitCode === null && daemon.signalCode === null) {
    daemon.kill('SIGTERM');
    await new Promise(resolve => daemon.once('exit', resolve));
  }
  rmSync(temp, { recursive: true, force: true });
}
