// Headless checks for src/lib/browserPoll.ts (pure logic). Run: node scripts/check-browser-poll.mjs
import { browserPollAction, browserPollRevived, browserButtonAction } from "../src/lib/browserPoll.ts";

let failures = 0;
function eq(actual, expected, label) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    failures++;
    console.error(`FAIL ${label}: got ${a}, want ${e}`);
  } else {
    console.log(`ok ${label}`);
  }
}

const up = { running: true, owned: true, engine_stale: false, url: "u" };
const down = { running: false, owned: false, engine_stale: false, url: "u" };

// daemon died under a live tab -> re-ensure once (the blank-iframe hole)
eq(browserPollAction(up, down), "ensure", "running->down ensures");
// still up -> adopt fresh state (stale flips included)
eq(browserPollAction(up, up), "update", "running->running updates");
eq(browserPollAction(down, up), "update", "down->running updates");
eq(browserPollAction(null, up), "update", "null->running updates");
// already down -> poll stays out (the visible-state ensure owns it)
eq(browserPollAction(down, down), "keep", "down->down keeps");
eq(browserPollAction(null, down), "keep", "null->down keeps");
// transport error -> keep the frame, retry next tick
eq(browserPollAction(up, null), "keep", "running->error keeps");
eq(browserPollAction(down, null), "keep", "down->error keeps");
eq(browserPollAction(null, null), "keep", "null->error keeps");

// revive re-navigates the frame (state-only updates leave it blank)
eq(browserPollRevived("ensure", true), true, "revive bumps frame");
eq(browserPollRevived("ensure", false), false, "failed ensure keeps frame");
eq(browserPollRevived("update", true), false, "update keeps frame");
eq(browserPollRevived("keep", false), false, "keep keeps frame");

// tab-bar button: down -> start, owned -> restart, adopted -> takeover
eq(browserButtonAction(null), "start", "button null starts");
eq(browserButtonAction(down), "start", "button down starts");
eq(browserButtonAction(up), "restart", "button owned restarts");
eq(browserButtonAction({ ...up, owned: false }), "takeover", "button adopted takes over");

if (failures) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("browserPoll: all green");
