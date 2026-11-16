import assert from "node:assert/strict";
import test from "node:test";
import { STOP_RECONCILE_DELAYS_MS, stopReconcileAction } from "../src/lib/stopReconcile.ts";

const base = { running: true, currentTurnId: "t1", stoppedTurnId: "t1", backendLive: false as boolean | null };

test("backend reports the stopped turn finished: clear the running mark", () => {
  assert.equal(stopReconcileAction(base), "clear");
});

test("backend still running it (cancel grace): look again", () => {
  assert.equal(stopReconcileAction({ ...base, backendLive: true }), "retry");
});

test("could not ask the backend: never guess it is over", () => {
  assert.equal(stopReconcileAction({ ...base, backendLive: null }), "retry");
});

test("already settled by the done event: nothing to do", () => {
  assert.equal(stopReconcileAction({ ...base, running: false }), "done");
  assert.equal(stopReconcileAction({ ...base, running: false, backendLive: true }), "done");
});

test("a newer turn owns the session: never clear it", () => {
  assert.equal(stopReconcileAction({ ...base, currentTurnId: "t2" }), "done");
  assert.equal(stopReconcileAction({ ...base, currentTurnId: undefined }), "done");
});

test("the last check lands past the engines' 20 s cancel grace", () => {
  assert.ok(STOP_RECONCILE_DELAYS_MS.at(-1)! > 20000);
  assert.deepEqual([...STOP_RECONCILE_DELAYS_MS], [...STOP_RECONCILE_DELAYS_MS].sort((a, b) => a - b));
});
