import assert from "node:assert/strict";
import test from "node:test";
import {
  bridgeAuthMode,
  bridgeConnLabel,
  bridgeConnState,
  bridgeConnTitle,
  bridgeDisplayName,
  DEFAULT_BRIDGE_ICON,
  safeIconPath,
  safeQrDataUrl,
} from "../src/lib/bridgePlugin.ts";

test("bridgeConnState classifies unconfigured, connected, and disconnected", () => {
  assert.equal(bridgeConnState(null), "unconfigured");
  assert.equal(bridgeConnState({ configured: false }), "unconfigured");
  assert.equal(
    bridgeConnState({ configured: true, enabled: true, connected: true, paired: { id: "1", name: "alice" } }),
    "connected"
  );
  assert.equal(
    bridgeConnState({ configured: true, enabled: true, polling: true, paired: { id: "1", name: "alice" } }),
    "connected"
  );
  assert.equal(
    bridgeConnState({ configured: true, enabled: false, connected: false, paired: { id: "1", name: "alice" } }),
    "disconnected"
  );
  assert.equal(
    bridgeConnState({ configured: true, enabled: true, connected: true, paired: null }),
    "disconnected"
  );
});

test("bridgeConnLabel and bridgeConnTitle format bridge name and state", () => {
  assert.equal(bridgeConnLabel("Discord", "connected"), "Discord connected");
  assert.equal(bridgeConnTitle("Discord", null), "Set up Discord bridge for this session");
  assert.equal(
    bridgeConnTitle("Discord", { configured: true, enabled: true, connected: true, paired: { id: "1", name: "a" }, botUsername: "vicuna_bot" }),
    "@vicuna_bot · Discord connected"
  );
  assert.equal(
    bridgeConnTitle("Discord", { configured: true, enabled: true, connected: false, paired: { id: "1", name: "a" }, botUsername: "vicuna_bot", lastError: "Gateway closed" }),
    "@vicuna_bot · Discord error: Gateway closed"
  );
});

test("safeIconPath accepts SVG path data and rejects injection", () => {
  assert.equal(safeIconPath("M12 2L2 22h20z"), "M12 2L2 22h20z");
  assert.equal(safeIconPath('M0 0"><script>'), DEFAULT_BRIDGE_ICON);
  assert.equal(safeIconPath(""), DEFAULT_BRIDGE_ICON);
  assert.equal(bridgeDisplayName({ name: "Discord Plugin", bridge: { name: "Discord" } }), "Discord");
  assert.equal(bridgeDisplayName({ name: "Discord", bridge: {} }), "Discord");
});

test("bridgeAuthMode defaults to token and accepts qr", () => {
  assert.equal(bridgeAuthMode(null), "token");
  assert.equal(bridgeAuthMode({}), "token");
  assert.equal(bridgeAuthMode({ authMode: "token" }), "token");
  assert.equal(bridgeAuthMode({ authMode: "qr" }), "qr");
});

test("safeQrDataUrl accepts base64 PNG/SVG data URLs and rejects unsafe values", () => {
  const png = "data:image/png;base64,iVBORw0KGgo=";
  const svg = "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=";
  assert.equal(safeQrDataUrl(png), png);
  assert.equal(safeQrDataUrl(`  ${svg}  `), svg);
  assert.equal(safeQrDataUrl("data:image/svg+xml,<svg onload=alert(1)>"), null);
  assert.equal(safeQrDataUrl("https://evil.example/qr.png"), null);
  assert.equal(safeQrDataUrl("javascript:alert(1)"), null);
  assert.equal(safeQrDataUrl("data:image/png;base64,not valid!!"), null);
  assert.equal(safeQrDataUrl("data:image/png;base64,"), null);
  assert.equal(safeQrDataUrl(null), null);
});
