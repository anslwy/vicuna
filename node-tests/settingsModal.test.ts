import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const root = new URL("..", import.meta.url).pathname;
const chatApp = readFileSync(root + "src/lib/SettingsModal.svelte", "utf8");

test("Settings General tab puts App updates first before Appearance", () => {
  const modalStart = chatApp.indexOf('data-testid="settings-modal"');
  assert.ok(modalStart >= 0, "settings-modal exists");
  const bodyStart = chatApp.indexOf('class="settings-pane-body', modalStart);
  const generalStart = chatApp.indexOf('{#if settingsTab === "general"}', bodyStart);
  const updatesIdx = chatApp.indexOf('data-testid="settings-updates-section"', generalStart);
  const appearanceIdx = chatApp.indexOf('data-testid="settings-appearance-section"', generalStart);
  assert.ok(updatesIdx > generalStart, "App updates section is in General");
  assert.ok(appearanceIdx > updatesIdx, "App updates comes before Appearance");
});

test("Settings groups Ollama, OpenRouter, and Continue & stops under Chat mode", () => {
  const chatNavStart = chatApp.indexOf('data-testid="settings-nav-chat"');
  assert.ok(chatNavStart >= 0, "Chat mode nav group exists");
  const chatNavEnd = chatApp.indexOf("</div>", chatNavStart);
  const chatNav = chatApp.slice(chatNavStart, chatNavEnd);
  assert.ok(chatNav.includes("Chat mode"), "Chat mode group label present");
  assert.ok(chatNav.includes('data-testid="settings-tab-ollama"'), "Ollama in Chat mode group");
  assert.ok(chatNav.includes('data-testid="settings-tab-openrouter"'), "OpenRouter in Chat mode group");
  assert.ok(chatNav.includes('data-testid="settings-tab-chat"'), "Continue & stops in Chat mode group");
  assert.ok(chatApp.includes('data-testid="settings-scope-chat"'), "Chat mode only scope badge exists");
});

test("Settings separates Work engine into Work tab and Ollama host into Ollama tab", () => {
  const modalStart = chatApp.indexOf('data-testid="settings-modal"');
  const bodyStart = chatApp.indexOf('class="settings-pane-body', modalStart);
  const generalStart = chatApp.indexOf('{#if settingsTab === "general"}', bodyStart);
  const workStart = chatApp.indexOf('{:else if settingsTab === "work"}', generalStart);
  const chatStart = chatApp.indexOf('{:else if settingsTab === "chat"}', workStart);
  const modelsStart = chatApp.indexOf('{:else if settingsTab === "models"}', chatStart);
  const generalBlock = chatApp.slice(generalStart, workStart);
  const workBlock = chatApp.slice(workStart, chatStart);
  const modelsBlock = chatApp.slice(modelsStart);

  assert.equal(generalBlock.includes('data-testid="settings-engine-picker"'), false, "Work engine moved out of General");
  assert.equal(generalBlock.includes('data-testid="ollama-host-input"'), false, "Ollama host moved out of General");
  assert.ok(workBlock.includes('data-testid="settings-engine-picker"'), "Work engine lives in Work tab");
  assert.ok(workBlock.includes('data-testid="browser-engine"'), "Browser engine lives in Work tab");
  assert.ok(modelsBlock.includes('data-testid="ollama-host-input"'), "Ollama host lives in Ollama tab");
});

test("Settings has a send-follow-ups-now toggle that defaults on", () => {
  const i = chatApp.indexOf('data-testid="work-send-now-toggle"');
  assert.ok(i >= 0, "toggle exists");
  const tag = chatApp.slice(chatApp.lastIndexOf("<input", i), chatApp.indexOf("/>", i));
  assert.ok(tag.includes("workEngineInfo?.sendNow ?? true"), "missing value = on");
  assert.ok(tag.includes("onToggleWorkSendNow"), "wired to the server setting");
});

test("Settings General tab has a desktop-only Show system tray icon toggle", () => {
  const modalStart = chatApp.indexOf('data-testid="settings-modal"');
  const bodyStart = chatApp.indexOf('class="settings-pane-body', modalStart);
  const generalStart = chatApp.indexOf('{#if settingsTab === "general"}', bodyStart);
  const workStart = chatApp.indexOf('{:else if settingsTab === "work"}', generalStart);
  const generalBlock = chatApp.slice(generalStart, workStart);
  const desktopIdx = generalBlock.indexOf("{#if isTauriRuntime()}");
  const toggleIdx = generalBlock.indexOf('data-testid="tray-icon-toggle"');
  assert.ok(desktopIdx >= 0, "Desktop & CLI section is gated by isTauriRuntime()");
  assert.ok(toggleIdx > desktopIdx, "tray toggle lives inside the desktop-only block in General");
  const tag = generalBlock.slice(generalBlock.lastIndexOf("<input", toggleIdx), generalBlock.indexOf("/>", toggleIdx));
  assert.ok(tag.includes("checked={trayEnabled}"), "bound to trayEnabled");
  assert.ok(tag.includes("onToggleTray"), "wired to onToggleTray");
  assert.ok(generalBlock.includes("Show system tray icon"), "label present");
});
