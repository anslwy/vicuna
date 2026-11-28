// Version ordering shared by the server menu (skew + upgrade detection).

/** Semver precedence incl. prerelease tags (`0.1.2` > `0.1.2-dev.2` >
 *  `0.1.2-dev.1` > `0.1.1`); mirrors `vicuna_lib::version::compare`.
 *  Returns <0, 0 or >0. */
export function cmpVersion(a: string, b: string): number {
  const p = (v: string) => {
    const [core, ...rest] = v.trim().replace(/^v/, "").split("+")[0].split("-");
    const pre = rest.join("-").split(".").filter(Boolean);
    return { core: core.split(".").map((x) => Number(x) || 0), pre };
  };
  const [x, y] = [p(a), p(b)];
  for (let i = 0; i < 3; i++) if ((x.core[i] ?? 0) !== (y.core[i] ?? 0)) return (x.core[i] ?? 0) - (y.core[i] ?? 0);
  if (!x.pre.length || !y.pre.length) return (x.pre.length ? 0 : 1) - (y.pre.length ? 0 : 1);
  const num = /^\d+$/;
  for (let i = 0; i < Math.min(x.pre.length, y.pre.length); i++) {
    const [m, n] = [x.pre[i], y.pre[i]];
    if (m === n) continue;
    if (num.test(m) && num.test(n)) return BigInt(m) < BigInt(n) ? -1 : 1;
    if (num.test(m)) return -1;
    if (num.test(n)) return 1;
    return m < n ? -1 : 1;
  }
  return x.pre.length - y.pre.length;
}

/** True when `v` carries a prerelease tag (`0.1.2-dev.<ts>`). */
export function isPrerelease(v: string): boolean {
  return /^v?\d+(\.\d+)*-./.test(v.trim().split("+")[0]);
}

/** Offer `latest` over `current`? Newer on either channel; on Stable also the
 *  stable release while running a dev prerelease (Dev → Stable switch, even
 *  when it sorts lower). Never an older stable. Mirrors
 *  `vicuna_lib::version::should_offer_update`. */
export function offersUpdate(channel: "stable" | "dev", latest: string, current: string): boolean {
  if (cmpVersion(latest, current) > 0) return true;
  return channel === "stable" && isPrerelease(current) && !isPrerelease(latest);
}

/** The offer is a Dev → Stable switch (target sorts below the running build). */
export function isChannelSwitch(channel: "stable" | "dev", latest: string, current: string): boolean {
  return offersUpdate(channel, latest, current) && cmpVersion(latest, current) < 0;
}
