import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const root = new URL("..", import.meta.url).pathname;
const chatApp = readFileSync(root + "src/lib/ChatApp.svelte", "utf8");
const pluginsModal = readFileSync(root + "src/lib/PluginsModal.svelte", "utf8");
const settingsModal = readFileSync(root + "src/lib/SettingsModal.svelte", "utf8");

test("sidebar footer orders Server switcher, Plugins, then Settings", () => {
  const footerStart = chatApp.indexOf('<div class="side-footer">');
  assert.ok(footerStart >= 0, "side-footer exists");
  const serverIdx = chatApp.indexOf('data-testid="server-btn"', footerStart);
  const pluginsIdx = chatApp.indexOf('data-testid="plugins-btn"', footerStart);
  const settingsIdx = chatApp.indexOf('data-testid="settings-btn"', footerStart);
  assert.ok(serverIdx > footerStart, "server-btn in side-footer");
  assert.ok(pluginsIdx > serverIdx, "plugins-btn comes after server-btn");
  assert.ok(settingsIdx > pluginsIdx, "settings-btn comes after plugins-btn");
});

test("collapsed rail includes Server switcher, Plugins, then Settings", () => {
  const railStart = chatApp.indexOf('<div class="collapsed-rail">');
  assert.ok(railStart >= 0, "collapsed-rail exists");
  const serverIdx = chatApp.indexOf('data-testid="server-btn-collapsed"', railStart);
  const pluginsIdx = chatApp.indexOf('data-testid="plugins-btn-collapsed"', railStart);
  const settingsIdx = chatApp.indexOf('data-testid="settings-btn-collapsed"', railStart);
  const menuIdx = chatApp.indexOf('{@render serverMenu()}', railStart);
  assert.ok(serverIdx > railStart, "server-btn-collapsed in collapsed-rail");
  assert.ok(menuIdx > railStart && menuIdx < serverIdx, "collapsed-rail renders serverMenu popup");
  assert.ok(pluginsIdx > serverIdx, "plugins-btn-collapsed comes after server-btn-collapsed");
  assert.ok(settingsIdx > pluginsIdx, "settings-btn-collapsed comes after plugins-btn-collapsed");
});

test("Settings modal no longer contains a Plugins tab", () => {
  assert.equal(settingsModal.includes('data-testid="settings-tab-plugins"'), false);
  assert.equal(settingsModal.includes('settingsTab === "plugins"'), false);
  assert.equal(settingsModal.includes("settingsTab === 'plugins'"), false);
  assert.ok(settingsModal.includes('data-testid="settings-tab-general"'));
  assert.ok(settingsModal.includes('data-testid="settings-tab-ollama"'));
  assert.ok(settingsModal.includes('data-testid="settings-tab-openrouter"'));
});

test("standalone Plugins modal hosts PluginsPanel with close controls", () => {
  assert.ok(chatApp.includes("<PluginsModal"), "ChatApp renders PluginsModal");
  const modalStart = pluginsModal.indexOf('data-testid="plugins-modal"');
  assert.ok(modalStart >= 0, "plugins-modal exists");
  assert.ok(pluginsModal.includes('data-testid="plugins-backdrop"'), "plugins-backdrop exists");
  assert.ok(pluginsModal.includes('data-testid="close-plugins-btn"'), "close-plugins-btn exists");
  const panelIdx = pluginsModal.indexOf("<PluginsPanel", modalStart);
  assert.ok(panelIdx > modalStart, "PluginsPanel renders inside plugins-modal");
});

test("Explore cards stay compact with 2-line descriptions and a Preview lightbox", () => {
  const pluginsPanel = readFileSync(root + "src/lib/PluginsPanel.svelte", "utf8");
  assert.equal(pluginsPanel.includes("pl-shot"), false, "inline screenshot banner removed");
  assert.ok(pluginsPanel.includes("-webkit-line-clamp: 2"), "descriptions clamp to 2 lines");
  assert.ok(pluginsPanel.includes("min-height: 3em"), "descriptions reserve 2-line height");
  assert.ok(pluginsPanel.includes('data-testid="plugin-card-preview-{e.id}"'), "Preview button testid present");
  assert.ok(pluginsPanel.includes('data-testid="plugin-preview-modal"'), "Preview lightbox modal present");
  assert.ok(pluginsPanel.includes("e.stopPropagation()"), "Escape stops propagation when lightbox closes");
});
