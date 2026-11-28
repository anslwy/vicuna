import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  normalizeUpdateChannel,
  updateManifestUrl,
  updateReleasesUrl,
  APP_UPDATE_LATEST_JSON_URL,
  APP_UPDATE_DEV_LATEST_JSON_URL
} from "../src/lib/appUpdate.ts";

const root = new URL("..", import.meta.url).pathname;

test("update channel normalizes to stable unless exactly dev", () => {
  assert.equal(normalizeUpdateChannel("dev"), "dev");
  assert.equal(normalizeUpdateChannel(" Dev "), "dev");
  assert.equal(normalizeUpdateChannel("stable"), "stable");
  assert.equal(normalizeUpdateChannel("nightly"), "stable");
  assert.equal(normalizeUpdateChannel(undefined), "stable");
});

test("each channel has its own manifest and release page", () => {
  assert.equal(updateManifestUrl("stable"), APP_UPDATE_LATEST_JSON_URL);
  assert.equal(APP_UPDATE_LATEST_JSON_URL, "https://github.com/anslwy/vicuna/releases/latest/download/latest.json");
  assert.equal(updateManifestUrl("dev"), "https://github.com/anslwy/vicuna/releases/download/latest/latest.json");
  assert.equal(APP_UPDATE_DEV_LATEST_JSON_URL, updateManifestUrl("dev"));
  assert.ok(updateReleasesUrl("dev").endsWith("/releases/tag/latest"));
});

test("Rust and tauri.conf agree on the endpoints", () => {
  const rs = readFileSync(root + "src-tauri/src/update_channel.rs", "utf8");
  assert.ok(rs.includes(updateManifestUrl("stable")));
  assert.ok(rs.includes(updateManifestUrl("dev")));
  const conf = JSON.parse(readFileSync(root + "src-tauri/tauri.conf.json", "utf8"));
  assert.deepEqual(conf.plugins.updater.endpoints, [updateManifestUrl("stable")]);
});

test("Settings General has the update channel control in the updates section", () => {
  const svelte = readFileSync(root + "src/lib/SettingsModal.svelte", "utf8");
  const start = svelte.indexOf('data-testid="settings-updates-section"');
  const end = svelte.indexOf("</section>", start);
  const block = svelte.slice(start, end);
  assert.ok(block.includes('data-testid="update-channel-select"'));
  assert.ok(block.includes('<option value="stable">Stable</option>'));
  assert.ok(block.includes('<option value="dev">Dev (pre-release)</option>'));
});

test("updates go through the channel-aware desktop commands", () => {
  const state = readFileSync(root + "src/lib/appUpdateState.svelte.ts", "utf8");
  assert.ok(state.includes('"app_check_update"') && state.includes('"app_install_update"'));
  assert.ok(!state.includes('import("@tauri-apps/plugin-updater")'), "JS check() can't switch endpoints");
  const bridge = readFileSync(root + "src/lib/bridge.ts", "utf8");
  for (const c of ["app_get_update_channel", "app_set_update_channel", "app_check_update", "app_install_update"]) {
    assert.ok(bridge.includes(`"${c}"`), c);
  }
});
