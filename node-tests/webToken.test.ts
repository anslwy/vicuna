import assert from "node:assert/strict";
import test from "node:test";
import { parseHashToken, stripHashToken } from "../src/lib/webToken.ts";

test("parseHashToken reads #token= and ignores junk", () => {
  assert.equal(parseHashToken("#token=0123456789abcdef"), "0123456789abcdef");
  assert.equal(parseHashToken("#x=1&token=abcdefgh12"), "abcdefgh12");
  assert.equal(parseHashToken(""), null);
  assert.equal(parseHashToken("#token="), null);
  assert.equal(parseHashToken("#token=short"), null);
  assert.equal(parseHashToken("#token=<script>alert(1)</script>"), null);
});

test("stripHashToken removes only the token", () => {
  assert.equal(stripHashToken("http://127.0.0.1:7422/#token=abcdefgh12"), "http://127.0.0.1:7422/");
  assert.equal(stripHashToken("http://h:9000/s/1#token=abcdefgh12&x=1"), "http://h:9000/s/1#x=1");
  assert.equal(stripHashToken("http://h:9000/s/1"), "http://h:9000/s/1");
});
