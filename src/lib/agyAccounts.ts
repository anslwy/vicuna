// Extra Antigravity accounts (backend: src-tauri/src/agy_accounts.rs). Each
// account is its own Work engine id `agy@<account-id>`; the default login is
// plain `agy`. These helpers group account rows under their harness for the
// engine picker and the Usage view.

export const AGY_ACCOUNT_PREFIX = "agy@";

export interface AgyAccountInfo {
  id: string;
  /** engine id, `agy@<id>` */
  engine: string;
  label: string;
  /** the account's own HOME on the machine running the turns */
  home: string;
  signedIn: boolean;
  /** `HOME='…' agy`: run it in any terminal on that machine, then sign in */
  signInCommand: string;
}

/** `agy@work` → `agy`; every other id is its own base. */
export function baseEngineId(id: string): string {
  return id.startsWith(AGY_ACCOUNT_PREFIX) ? "agy" : id;
}

export function isAccountEngine(id: string): boolean {
  return id.startsWith(AGY_ACCOUNT_PREFIX);
}

/** Harnesses that can stack extra accounts (picker shows a + on their row). */
export function supportsAccounts(id: string): boolean {
  return id === "agy";
}

/** Usage view: one tab per harness, plus account chips for the selected
 *  harness when it has extra accounts (`Default` = the base engine). */
export function usageAccountView<T extends { id: string; label: string; account?: AgyAccountInfo | null }>(
  tabs: T[],
  selected: string,
): { harnessTabs: T[]; selectedBase: string; chips: { id: string; label: string }[] } {
  const harnessTabs = tabs.filter((t) => !isAccountEngine(t.id));
  const selectedBase = baseEngineId(selected);
  const accounts = tabs.filter((t) => isAccountEngine(t.id) && baseEngineId(t.id) === selectedBase);
  const chips = accounts.length
    ? [{ id: selectedBase, label: "Default" }, ...accounts.map((a) => ({ id: a.id, label: a.account?.label ?? a.id.slice(AGY_ACCOUNT_PREFIX.length) }))]
    : [];
  return { harnessTabs, selectedBase, chips };
}
