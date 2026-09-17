const MAX_OPEN_PRS = 5;

// Use the REST list endpoint rather than search, whose index can lag new PRs.
async function openPulls(github, repo) {
  const pulls = await github.paginate(github.rest.pulls.list, {
    ...repo,
    state: 'open',
    sort: 'created',
    direction: 'asc',
    per_page: 100,
  });
  return pulls.sort((a, b) =>
    a.created_at.localeCompare(b.created_at) || a.number - b.number);
}

async function commentOnce(github, repo, number, marker, body) {
  const comments = await github.paginate(github.rest.issues.listComments, {
    ...repo, issue_number: number, per_page: 100,
  });
  if (!comments.some(comment =>
    comment.user?.login === 'github-actions[bot]' && comment.body?.includes(marker))) {
    await github.rest.issues.createComment({
      ...repo, issue_number: number, body: `${marker}\n${body}`,
    });
  }
}

module.exports = async function enforceLimit({ github, context, core, dryRun = false }) {
  const repo = context.repo;
  const initial = await openPulls(github, repo);
  const authors = new Set(initial.map(pull => pull.user.id));
  for (const author of authors) {
    const candidates = initial.filter(pull => pull.user.id === author);
    for (const candidate of candidates.slice(0, Math.max(0, candidates.length - MAX_OPEN_PRS))) {
      // Another actor may have closed PRs while this workflow was queued.
      const current = dryRun ? candidates
        : (await openPulls(github, repo)).filter(pull => pull.user.id === author);
      const excess = current.slice(0, Math.max(0, current.length - MAX_OPEN_PRS));
      if (!excess.some(pull => pull.number === candidate.number)) continue;
      const newest = current.at(-1);
      core.info(`${dryRun ? '[dry-run] ' : ''}Close #${candidate.number}; author has ${current.length} open PRs; newest #${newest.number}`);
      if (dryRun) continue;

      const marker = `<!-- pr-limit:${candidate.number}:${newest.number} -->`;
      // Write both explanations before closing so a partial failure can be retried.
      await commentOnce(github, repo, candidate.number, marker,
        `本仓库每位作者最多保留 ${MAX_OPEN_PRS} 个开放 PR（包含 Draft）。当前已有 ${current.length} 个，最新 PR 为 #${newest.number}。本 PR 按创建时间属于超额的最老 PR，将自动关闭，分支会保留。继续此项工作前，请先合并或关闭其他 PR 以腾出名额；重新打开仍会检查数量。`);
      await commentOnce(github, repo, newest.number, marker,
        `本仓库每位作者最多保留 ${MAX_OPEN_PRS} 个开放 PR（包含 Draft）。当前已有 ${current.length} 个，因此将按创建时间自动关闭较早的 #${candidate.number}，并保留分支。此限制按作者跨目标分支统计。`);

      const refreshed = (await openPulls(github, repo)).filter(pull => pull.user.id === author);
      if (!refreshed.slice(0, Math.max(0, refreshed.length - MAX_OPEN_PRS))
        .some(pull => pull.number === candidate.number)) {
        core.info(`Skip #${candidate.number}: it is no longer over the limit`);
        continue;
      }
      await github.rest.pulls.update({ ...repo, pull_number: candidate.number, state: 'closed' });
    }
  }
};

// Run self-tests only as an entry point, never when loaded by github-script.
if (require.main === module) {
  const assert = require('node:assert/strict');
  const test = require('node:test');
  const enforceLimit = module.exports;

  function pull(number, author = 10) {
    return {
      number, user: { id: author }, state: 'open', draft: number % 2 === 0,
      created_at: new Date(Date.UTC(2026, 0, number)).toISOString(),
    };
  }

  // In-memory API boundary; all selection and retry decisions use production code.
  function fixture(pulls) {
    const comments = new Map();
    const closed = [];
    const rest = {
      pulls: {
        list: async () => ({ data: pulls.filter(p => p.state === 'open') }),
        update: async ({ pull_number, state }) => {
          assert.equal(state, 'closed');
          pulls.find(p => p.number === pull_number).state = state;
          closed.push(pull_number);
        },
      },
      issues: {
        listComments: async ({ issue_number }) => ({ data: comments.get(issue_number) || [] }),
        createComment: async ({ issue_number, body }) => {
          comments.set(issue_number, [...(comments.get(issue_number) || []),
            { body, user: { login: 'github-actions[bot]' } }]);
        },
      },
    };
    const args = {
      github: { rest, paginate: async (method, params) => (await method(params)).data },
      context: { repo: { owner: 'example', repo: 'project' } },
      core: { info() {} },
    };
    return { args, rest, comments, closed };
  }

  test('keeps the newest five per author, including drafts, and explains both sides', async () => {
    const pulls = [...Array.from({ length: 7 }, (_, i) => pull(i + 1)),
      ...Array.from({ length: 5 }, (_, i) => pull(i + 10, 20))].reverse();
    const f = fixture(pulls);
    await enforceLimit(f.args);
    assert.deepEqual(f.closed, [1, 2]);
    assert.equal(pulls.filter(p => p.user.id === 10 && p.state === 'open').length, 5);
    assert.match(f.comments.get(1)[0].body, /#7/);
    assert.match(f.comments.get(2)[0].body, /#7/);
    assert.match(f.comments.get(7)[0].body, /#1/);
    assert.match(f.comments.get(7)[1].body, /#2/);
    await enforceLimit(f.args);
    assert.deepEqual(f.closed, [1, 2]);

    // Reopening an old PR must not make it newer than its original creation date.
    pulls.find(p => p.number === 1).state = 'open';
    await enforceLimit(f.args);
    assert.deepEqual(f.closed, [1, 2, 1]);
    assert.equal(f.comments.get(7).length, 2);
  });

  test('dry-run reports candidates without any writes', async () => {
    const f = fixture(Array.from({ length: 7 }, (_, i) => pull(i + 1)));
    const messages = [];
    f.args.core.info = message => messages.push(message);
    await enforceLimit({ ...f.args, dryRun: true });
    assert.equal(messages.length, 2);
    assert.ok(messages.every(message => message.startsWith('[dry-run]')));
    assert.deepEqual(f.closed, []);
    assert.equal(f.comments.size, 0);
  });

  test('partial notification or close failures can be retried without duplicate comments', async () => {
    for (const operation of ['comment', 'close']) {
      const f = fixture(Array.from({ length: 6 }, (_, i) => pull(i + 1)));
      const create = f.rest.issues.createComment;
      const update = f.rest.pulls.update;
      if (operation === 'comment') {
        f.rest.issues.createComment = async params => {
          if (params.issue_number === 6) throw new Error('API unavailable');
          return create(params);
        };
      } else {
        f.rest.pulls.update = async () => { throw new Error('API unavailable'); };
      }
      await assert.rejects(enforceLimit(f.args), /API unavailable/);
      assert.deepEqual(f.closed, []);
      f.rest.issues.createComment = create;
      f.rest.pulls.update = update;
      await enforceLimit(f.args);
      assert.deepEqual(f.closed, [1]);
      assert.equal(f.comments.get(1).length, 1);
      assert.equal(f.comments.get(6).length, 1);
    }
  });

  test('rechecks the quota when a PR is closed during notification', async () => {
    const pulls = Array.from({ length: 6 }, (_, i) => pull(i + 1));
    const f = fixture(pulls);
    const create = f.rest.issues.createComment;
    f.rest.issues.createComment = async params => {
      await create(params);
      pulls.find(p => p.number === 6).state = 'closed';
    };
    await enforceLimit(f.args);
    assert.deepEqual(f.closed, []);
    assert.equal(pulls[0].state, 'open');
  });

  test('an incomplete API read fails without closing PRs', async () => {
    const f = fixture(Array.from({ length: 6 }, (_, i) => pull(i + 1)));
    f.rest.pulls.list = async () => { throw new Error('pagination failed'); };
    await assert.rejects(enforceLimit(f.args), /pagination failed/);
    assert.deepEqual(f.closed, []);
    assert.equal(f.comments.size, 0);
  });
}
