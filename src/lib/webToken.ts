// `vicuna web` hands the browser its server token in the URL fragment
// (`/#token=<t>`); these pure helpers read and strip it (src/lib/bridge.ts).

/** `#token=<t>` in a URL fragment (what `vicuna web` opens), else null. */
export function parseHashToken(hash: string): string | null {
  const h = hash.replace(/^#/, "");
  for (const kv of h.split("&")) {
    const [k, v] = kv.split("=");
    if (k === "token" && v && /^[A-Za-z0-9._~-]{8,256}$/.test(v)) return v;
  }
  return null;
}

/** The URL without its `token=` fragment entry (other fragment parts kept). */
export function stripHashToken(href: string): string {
  const i = href.indexOf("#");
  if (i < 0) return href;
  const rest = href.slice(i + 1).split("&").filter((kv) => kv.split("=")[0] !== "token").join("&");
  return href.slice(0, i) + (rest ? `#${rest}` : "");
}
