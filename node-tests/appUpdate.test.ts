// In-app updater helpers, ported from cojudge's src/lib/appUpdate.test.ts (run: `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  APP_UPDATE_LATEST_JSON_URL,
  APP_UPDATE_RELEASES_URL,
  createReleaseNotesPreview,
  formatAppUpdateInstallError,
  normalizeUpdateErrorMessage
} from "../src/lib/appUpdate.ts";

test("normalizes quoted and prefixed errors", () => {
  assert.equal(normalizeUpdateErrorMessage('"hello"'), "hello");
  assert.equal(normalizeUpdateErrorMessage("Error: boom"), "boom");
  assert.equal(normalizeUpdateErrorMessage('  Error: "spaced"  '), "spaced");
  assert.equal(normalizeUpdateErrorMessage(""), "Unknown error");
});

test("strips markdown for release-notes preview", () => {
  assert.equal(createReleaseNotesPreview(null), null);
  assert.equal(createReleaseNotesPreview("## Title\n- item"), "Title\nitem");
  const long = "a".repeat(300);
  const preview = createReleaseNotesPreview(long);
  assert.ok(preview?.endsWith("..."));
  assert.ok(preview!.length < long.length);
});

test("formats install errors with manual fallback", () => {
  const message = formatAppUpdateInstallError(new Error("econnreset while downloading"));
  assert.match(message, /Vicuna/);
  assert.ok(message.includes(APP_UPDATE_RELEASES_URL));
});

test("update manifest lives on the newest stable GitHub release", () => {
  assert.equal(APP_UPDATE_LATEST_JSON_URL, "https://github.com/anslwy/vicuna/releases/latest/download/latest.json");
});
