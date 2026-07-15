const assert = require("node:assert/strict");
const test = require("node:test");

const { createSettingsTransactionQueue, readSecretInput, updateSecretInputChangeState } = require("./admin.js");

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test("settings transactions serialize their complete UI completion", async () => {
  const enqueue = createSettingsTransactionQueue();
  const firstPost = deferred();
  const events = [];
  const ui = { rendered: "", message: "", baseline: "" };

  const runTransaction = (name, post) =>
    enqueue(async () => {
      events.push(`${name}:get`);
      await post;
      events.push(`${name}:post`);
      events.push(`${name}:refresh`);
      ui.rendered = name;
      ui.message = `${name} saved`;
      ui.baseline = name;
    });

  const older = runTransaction("older", firstPost.promise);
  const newer = runTransaction("newer", Promise.resolve());

  await Promise.resolve();
  assert.deepEqual(events, ["older:get"]);

  firstPost.resolve();
  await Promise.all([older, newer]);

  assert.deepEqual(events, [
    "older:get",
    "older:post",
    "older:refresh",
    "newer:get",
    "newer:post",
    "newer:refresh",
  ]);
  assert.deepEqual(ui, {
    rendered: "newer",
    message: "newer saved",
    baseline: "newer",
  });
});

test("secret inputs preserve whitespace and detect verbatim changes", () => {
  const input = {
    value: "  secret with surrounding whitespace\t",
    dataset: {
      secretClear: "1",
      secretConfigured: "1",
      secretMasked: "0",
    },
  };

  assert.equal(readSecretInput(input), "  secret with surrounding whitespace\t");
  assert.equal(updateSecretInputChangeState(input), true);
  assert.deepEqual(input.dataset, {
    secretClear: "0",
    secretConfigured: "0",
    secretMasked: "0",
  });

  assert.equal(readSecretInput({ value: "", dataset: {} }), "");
  assert.equal(readSecretInput({ value: "••••••••••••", dataset: {} }), "");
  assert.equal(readSecretInput({ value: "replacement", dataset: { secretMasked: "1" } }), "");
});
