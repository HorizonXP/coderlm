const MIXED_GREP_TARGET = "mixed-fixture-grep";

function dispatchJob(name, payload) {
  return { name, payload, marker: MIXED_GREP_TARGET };
}

function runWorker(queue) {
  return queue.map((item) => dispatchJob("fixture-job", item));
}

module.exports = { dispatchJob, runWorker };
