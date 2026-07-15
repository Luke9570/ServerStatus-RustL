const assert = require("node:assert/strict");
const test = require("node:test");

const { createSettingsTransactionQueue } = require("./admin.js");

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
