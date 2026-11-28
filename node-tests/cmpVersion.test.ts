import { test } from "node:test";
import assert from "node:assert/strict";
import { cmpVersion } from "../src/lib/version.ts";

test("stable versions compare numerically", () => {
  assert.ok(cmpVersion("v0.0.10", "0.0.9") > 0);
  assert.ok(cmpVersion("0.1.2", "0.1.1") > 0);
  assert.equal(cmpVersion("v0.1.1", "0.1.1"), 0);
});

test("dev prereleases order like semver", () => {
  assert.ok(cmpVersion("0.1.2-dev.20261128155414", "0.1.1") > 0);
  assert.ok(cmpVersion("0.1.2-dev.20261128160000", "0.1.2-dev.20261128155414") > 0);
  assert.ok(cmpVersion("0.1.2", "0.1.2-dev.20261128160000") > 0);
  assert.ok(cmpVersion("0.1.2-dev.20261128160000", "0.1.2") < 0);
  assert.equal(cmpVersion("0.1.2-dev.20261128155414", "0.1.2-dev.20261128155414"), 0);
  assert.ok(cmpVersion("0.1.1", "0.1.2-dev.20261128155414") < 0);
  // numeric identifiers beyond Number precision still compare exactly
  assert.ok(cmpVersion("1.0.0-dev.99999999999999999", "1.0.0-dev.99999999999999998") > 0);
  assert.ok(cmpVersion("1.0.0-alpha", "1.0.0-alpha.1") < 0);
  assert.ok(cmpVersion("1.0.0-alpha.1", "1.0.0-beta") < 0);
});

import { isPrerelease, offersUpdate, isChannelSwitch } from "../src/lib/version.ts";

test("Dev -> Stable switch is offered, stable never downgrades", () => {
  const dev = "0.1.2-dev.20261128155414";
  assert.ok(isPrerelease(dev));
  assert.ok(!isPrerelease("v0.1.1"));
  assert.ok(offersUpdate("stable", "0.1.1", dev));
  assert.ok(isChannelSwitch("stable", "0.1.1", dev));
  assert.ok(offersUpdate("stable", "0.1.2", dev));
  assert.ok(!isChannelSwitch("stable", "0.1.2", dev));
  assert.ok(!offersUpdate("stable", "0.1.1", "0.1.1"));
  assert.ok(!offersUpdate("stable", "0.1.0", "0.1.1"));
  assert.ok(offersUpdate("stable", "0.1.2", "0.1.1"));
  assert.ok(offersUpdate("dev", dev, "0.1.1"));
  assert.ok(!offersUpdate("dev", "0.1.1", dev));
  assert.ok(!offersUpdate("dev", "0.1.2-dev.20261128151100", dev));
  assert.ok(offersUpdate("dev", dev, "0.1.2-dev.20261128151100"));
});
