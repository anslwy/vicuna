import assert from "node:assert/strict";
import test from "node:test";
import { telegramConnLabel, telegramConnState, telegramConnTitle } from "../src/lib/telegram.ts";

test("unconfigured when status is missing or no bot is configured", () => {
  assert.equal(telegramConnState(null), "unconfigured");
  assert.equal(telegramConnState(undefined), "unconfigured");
  assert.equal(telegramConnState({}), "unconfigured");
  assert.equal(
    telegramConnState({ configured: false, enabled: false, polling: false, paired: null }),
    "unconfigured"
  );
  assert.equal(
    telegramConnState({ configured: false, enabled: true, polling: true, paired: { id: 1, name: "A" } }),
    "unconfigured"
  );
});

test("connected only when configured, enabled, polling, and paired", () => {
  assert.equal(
    telegramConnState({ configured: true, enabled: true, polling: true, paired: { id: 42, name: "Alice" } }),
    "connected"
  );
});

test("disconnected when configured but disabled, not polling, or unpaired", () => {
  assert.equal(
    telegramConnState({ configured: true, enabled: false, polling: false, paired: { id: 42, name: "Alice" } }),
    "disconnected"
  );
  assert.equal(
    telegramConnState({ configured: true, enabled: true, polling: false, paired: { id: 42, name: "Alice" } }),
    "disconnected"
  );
  assert.equal(
    telegramConnState({ configured: true, enabled: true, polling: true, paired: null }),
    "disconnected"
  );
  assert.equal(
    telegramConnState({ configured: true, enabled: false, polling: false, paired: null }),
    "disconnected"
  );
});

test("labels describe each connection state", () => {
  assert.equal(telegramConnLabel("connected"), "Telegram connected");
  assert.equal(telegramConnLabel("disconnected"), "Telegram disconnected");
  assert.equal(telegramConnLabel("unconfigured"), "Telegram not set up");
});

test("titles include bot username and specific disconnected reason", () => {
  assert.equal(telegramConnTitle(null), "Set up Telegram bridge for this session");
  assert.equal(
    telegramConnTitle({ configured: true, enabled: true, polling: true, paired: { id: 1 }, botUsername: "alpha_bot" }),
    "@alpha_bot · Telegram connected"
  );
  assert.equal(
    telegramConnTitle({ configured: true, enabled: false, polling: false, paired: { id: 1 }, botUsername: "alpha_bot" }),
    "@alpha_bot · Telegram paused"
  );
  assert.equal(
    telegramConnTitle({ configured: true, enabled: true, polling: true, paired: null, botUsername: "alpha_bot" }),
    "@alpha_bot · Telegram waiting to pair"
  );
  assert.equal(
    telegramConnTitle({ configured: true, enabled: true, polling: false, paired: { id: 1 }, botUsername: "alpha_bot", lastError: "409 Conflict" }),
    "@alpha_bot · Telegram error: 409 Conflict"
  );
});
