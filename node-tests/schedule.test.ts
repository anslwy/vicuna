// Scheduled-send rules (run: `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  schedulePresets,
  parseCustomTime,
  customTimeError,
  defaultCustomTime,
  dateInputValue,
  timeInputValue,
  formatWhen,
  formatClock,
  dueItems,
  markMissed,
  parseScheduled,
  sortScheduled,
  previewText,
  trayScheduleLines,
  scheduledCountLabel,
  type ScheduledItem
} from "../src/lib/schedule.ts";

const at = (y: number, mo: number, d: number, h: number, mi: number) => new Date(y, mo - 1, d, h, mi).getTime();
const NOW = at(2026, 11, 12, 20, 0);

function item(over: Partial<ScheduledItem>): ScheduledItem {
  return { id: "a", chatId: "c1", title: "Session", workspace: "/ws", text: "run the tests", at: NOW + 60_000, createdAt: NOW, ...over };
}

test("presets are 30 minutes, 1 hour and 3 hours ahead", () => {
  const p = schedulePresets(NOW);
  assert.deepEqual(p.map((x) => x.label), ["In 30 minutes", "In 1 hour", "In 3 hours"]);
  assert.deepEqual(p.map((x) => x.at - NOW), [30 * 60_000, 60 * 60_000, 180 * 60_000]);
});

test("custom time parses local date + time and round-trips the input values", () => {
  const ms = parseCustomTime("2026-11-13", "09:05");
  assert.equal(ms, at(2026, 11, 13, 9, 5));
  assert.equal(dateInputValue(ms!), "2026-11-13");
  assert.equal(timeInputValue(ms!), "09:05");
});

test("custom time rejects junk and impossible dates", () => {
  assert.equal(parseCustomTime("", "09:00"), null);
  assert.equal(parseCustomTime("2026-11-13", ""), null);
  assert.equal(parseCustomTime("2026-02-31", "09:00"), null);
  assert.equal(parseCustomTime("2026-11-13", "25:00"), null);
});

test("custom time must be in the future", () => {
  assert.equal(customTimeError(null, NOW), "Pick a date and a time.");
  assert.equal(customTimeError(NOW, NOW), "Pick a time in the future.");
  assert.equal(customTimeError(NOW - 1, NOW), "Pick a time in the future.");
  assert.equal(customTimeError(NOW + 1, NOW), null);
});

test("default custom time is about an hour ahead on a 5-minute mark", () => {
  const d = defaultCustomTime(at(2026, 11, 12, 20, 2));
  assert.equal(d, at(2026, 11, 12, 21, 5));
  assert.equal(defaultCustomTime(NOW), at(2026, 11, 12, 21, 0));
});

test("formatWhen names today, tomorrow, yesterday and other days", () => {
  assert.equal(formatWhen(at(2026, 11, 12, 21, 30), NOW), "Today 9:30 PM");
  assert.equal(formatWhen(at(2026, 11, 13, 9, 0), NOW), "Tomorrow 9:00 AM");
  assert.equal(formatWhen(at(2026, 11, 11, 18, 0), NOW), "Yesterday 6:00 PM");
  assert.equal(formatWhen(at(2026, 11, 20, 0, 5), NOW), "Nov 20, 12:05 AM");
  assert.equal(formatWhen(at(2027, 1, 2, 12, 0), NOW), "Jan 2 2027, 12:00 PM");
  assert.equal(formatClock(at(2026, 11, 12, 12, 0)), "12:00 PM");
});

test("only non-missed items at or past their time are due, soonest first", () => {
  const items = [
    item({ id: "later", at: NOW + 1 }),
    item({ id: "b", at: NOW - 10 }),
    item({ id: "a", at: NOW - 20 }),
    item({ id: "missed", at: NOW - 30, missed: true }),
    item({ id: "exact", at: NOW })
  ];
  assert.deepEqual(dueItems(items, NOW).map((x) => x.id), ["a", "b", "exact"]);
});

test("launch marks already-due items missed and leaves future ones alone", () => {
  const out = markMissed([item({ id: "old", at: NOW - 1 }), item({ id: "new", at: NOW + 1 })], NOW);
  assert.equal(out[0].missed, true);
  assert.equal(out[1].missed, undefined);
  // a missed item never fires on its own
  assert.deepEqual(dueItems(out, NOW + 10).map((x) => x.id), ["new"]);
});

test("missed items sort first", () => {
  const out = sortScheduled([item({ id: "soon", at: NOW + 1 }), item({ id: "missed", at: NOW - 5, missed: true })]);
  assert.deepEqual(out.map((x) => x.id), ["missed", "soon"]);
});

test("stored lists survive a round trip and drop malformed rows", () => {
  const good = item({ id: "x", images: ["data:image/png;base64,AA"], missed: true, error: "no engine" });
  const raw = JSON.stringify([good, { id: "no-time", workspace: "/ws", text: "t" }, { id: "empty", at: 1, workspace: "/ws", text: "" }, { id: "no-ws", at: 1, text: "t" }, null, 3]);
  assert.deepEqual(parseScheduled(raw), [good]);
  assert.deepEqual(parseScheduled("not json"), []);
  assert.deepEqual(parseScheduled('{"a":1}'), []);
  assert.deepEqual(parseScheduled(null), []);
});

test("previews flatten whitespace, truncate, and name image-only messages", () => {
  assert.equal(previewText({ text: "  fix\n the   build " }), "fix the build");
  assert.equal(previewText({ text: "x".repeat(80) }, 10), "x".repeat(9) + "…");
  assert.equal(previewText({ text: "", images: ["data:x"] }), "Image");
});

test("tray lines are capped with a +N more row and flag missed items", () => {
  const items = [1, 2, 3].map((n) => item({ id: `i${n}`, at: at(2026, 11, 12, 20 + n, 0), text: `task ${n}` }));
  assert.deepEqual(trayScheduleLines(items, NOW, 2), ["Today 9:00 PM · task 1", "Today 10:00 PM · task 2", "+1 more"]);
  assert.deepEqual(trayScheduleLines([item({ at: at(2026, 11, 12, 18, 0), missed: true, text: "late" })], NOW), ["Missed Today 6:00 PM · late"]);
  assert.equal(scheduledCountLabel(1), "1 scheduled message");
  assert.equal(scheduledCountLabel(2), "2 scheduled messages");
});
