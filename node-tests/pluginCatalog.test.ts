import assert from "node:assert/strict";
import test from "node:test";
import { cardAction, filterCatalog, githubRepoUrl, safeImageUrl, type CatalogEntry } from "../src/lib/pluginCatalog.ts";

const entry = (over: Partial<CatalogEntry> = {}): CatalogEntry => ({
  id: "lagoon", name: "Lagoon Themes", description: "Deep teal dark theme.", author: "anslwy", source: "anslwy/vicuna-theme-lagoon",
  kinds: ["theme"], tags: ["teal"], installed: null, conflict: false, compatible: true, ...over
});

test("image URLs: https GitHub only", () => {
  for (const ok of ["https://raw.githubusercontent.com/a/b/main/i.svg", "https://github.com/a/b/raw/main/s.jpg"]) assert.equal(safeImageUrl(ok), ok);
  for (const bad of [
    "http://raw.githubusercontent.com/a.svg", "https://evil.example/i.svg", "https://raw.githubusercontent.com.evil.example/x",
    "javascript:alert(1)", "data:image/svg+xml,<svg/>", "https://github.com/a\" onerror=\"x", "https://github.com//evil.example/x",
    "https://github.com/a b", 42, null
  ]) assert.equal(safeImageUrl(bad), null, String(bad));
});

test("search matches every word across fields", () => {
  const list = [entry(), entry({ id: "scratchpad", name: "Scratchpad", description: "Notes panel.", kinds: ["panel"], tags: ["notes"] })];
  assert.deepEqual(filterCatalog(list, "").map((e) => e.id), ["lagoon", "scratchpad"]);
  assert.deepEqual(filterCatalog(list, "NOTES").map((e) => e.id), ["scratchpad"]);
  assert.deepEqual(filterCatalog(list, "panel notes").map((e) => e.id), ["scratchpad"]);
  assert.deepEqual(filterCatalog(list, "teal anslwy").map((e) => e.id), ["lagoon"]);
  assert.deepEqual(filterCatalog(list, "nothing"), []);
});

test("card action", () => {
  assert.equal(cardAction(entry()), "install");
  assert.equal(cardAction(entry({ installed: "1.0.0" })), "upgrade");
  assert.equal(cardAction(entry({ installed: "1.0.0", conflict: true })), "conflict");
  assert.equal(cardAction(entry({ compatible: false })), "incompatible");
});

test("githubRepoUrl resolves GitHub sources and ignores local/non-GitHub sources", () => {
  const expected = "https://github.com/anslwy/vicuna-theme-lagoon";
  for (const src of [
    "anslwy/vicuna-theme-lagoon",
    "  anslwy/vicuna-theme-lagoon/  ",
    "anslwy/vicuna-theme-lagoon.git",
    "github.com/anslwy/vicuna-theme-lagoon",
    "https://github.com/anslwy/vicuna-theme-lagoon",
    "https://github.com/anslwy/vicuna-theme-lagoon/",
    "https://github.com/anslwy/vicuna-theme-lagoon.git",
    "http://github.com/anslwy/vicuna-theme-lagoon",
    "HTTPS://GITHUB.COM/anslwy/vicuna-theme-lagoon.GIT",
  ]) {
    assert.equal(githubRepoUrl(src), expected, src);
  }
  assert.equal(githubRepoUrl("my_org/my.plugin-1"), "https://github.com/my_org/my.plugin-1");
  for (const nonGh of [
    "",
    "   ",
    null,
    undefined,
    42,
    "/home/user/my-plugin",
    "./my-plugin",
    "../my-plugin",
    "file:///home/user/my-plugin",
    "C:\\plugins\\my-plugin",
    "C:/plugins/my-plugin",
    "https://example.com/vicuna-plugin.json",
    "https://github.com/anslwy/vicuna-theme-lagoon/releases/download/v1.0.0/vicuna-plugin.json",
    "https://gitlab.com/anslwy/vicuna-theme-lagoon",
    "anslwy/vicuna/extra",
    "only-one-segment",
    "../evil",
    "owner/..",
    "owner/repo with space",
    "owner/repo?q=1",
  ]) {
    assert.equal(githubRepoUrl(nonGh), null, String(nonGh));
  }
});
