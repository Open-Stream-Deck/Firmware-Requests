const { test } = require("node:test");
const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");

const workflow = readFileSync(join(__dirname, "../workflows/retry-runners.yml"), "utf8");
const script = workflow.split("          script: |\n")[1].replace(/^ {12}/gm, "");
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const execute = new AsyncFunction("github", "context", "core", script);

test("retry workflow only handles failed runs with attempts remaining", () => {
  assert.match(workflow, /conclusion == 'failure' && github.event.workflow_run.run_attempt < 3/);
  assert.doesNotMatch(workflow, /actions\/checkout/);
});

for (const [name, jobs, current, expected] of [
  ["unassigned cancelled runner", [{ conclusion: "cancelled", runner_id: 0, steps: [] }], {}, 1],
  ["unassigned failed runner", [{ conclusion: "failure", runner_id: 0, steps: [] }], {}, 1],
  ["successful jobs stay untouched", [{ conclusion: "success", runner_id: 1, steps: [{}] }, { conclusion: "cancelled", runner_id: 0, steps: [] }], {}, 1],
  ["compile failure", [{ conclusion: "failure", runner_id: 1, steps: [{}] }], {}, 0],
  ["mixed code and runner failures", [{ conclusion: "failure", runner_id: 1, steps: [{}] }, { conclusion: "cancelled", runner_id: 0, steps: [] }], {}, 0],
  ["skipped dependency", [{ conclusion: "skipped", runner_id: null, steps: [] }], {}, 0],
  ["newer attempt", [], { run_attempt: 2 }, 0],
  ["manual rerun in progress", [], { status: "in_progress" }, 0],
]) {
  test(name, async () => {
    let retries = 0;
    const params = { owner: "owner", repo: "repo", run_id: 42 };
    const actions = {
      getWorkflowRun: async () => ({ data: { status: "completed", run_attempt: 1, ...current } }),
      listJobsForWorkflowRunAttempt: {},
      reRunWorkflowFailedJobs: async actual => { assert.deepEqual(actual, params); retries++; },
    };
    await execute(
      { rest: { actions }, paginate: async (endpoint, actual) => {
        assert.equal(endpoint, actions.listJobsForWorkflowRunAttempt);
        assert.deepEqual(actual, { ...params, attempt_number: 1, per_page: 100 });
        return jobs;
      } },
      { repo: { owner: "owner", repo: "repo" }, payload: { workflow_run: { id: 42, run_attempt: 1 } } },
      { info() {} },
    );
    assert.equal(retries, expected);
  });
}
