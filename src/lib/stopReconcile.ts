/** After the user stops a Work turn, the window must stop showing it as
 *  running even when the backend's `done` event never reaches it (dropped
 *  channel, a settle that bailed on a turn-seq mismatch, …). The backend is
 *  asked directly (`work_agent_turn_live`) and this decides what to do with
 *  the answer. Pure so it is node-tested; ChatApp owns the side effects. */

export type StopReconcileAction =
  /** the backend says the stopped turn is over: settle the session now */
  | "clear"
  /** still live (or we could not ask): look again later */
  | "retry"
  /** nothing to do: already settled, or a newer turn owns the session */
  | "done";

export interface StopReconcileInput {
  /** the window still marks this session as running a Work turn */
  running: boolean;
  /** the backend turn id the window currently tracks for the session */
  currentTurnId: string | undefined;
  /** the backend turn id the user stopped */
  stoppedTurnId: string;
  /** `work_agent_turn_live` for the stopped turn; null = could not ask
   *  (older shell/server without the command, bridge unreachable) */
  backendLive: boolean | null;
}

export function stopReconcileAction(s: StopReconcileInput): StopReconcileAction {
  if (!s.running) return "done";
  // a newer turn started in this session since the stop: never clear it
  if (s.currentTurnId !== s.stoppedTurnId) return "done";
  // unknown is not "over": an old backend must never make us lie the other way
  if (s.backendLive !== false) return "retry";
  return "clear";
}

/** When to ask after a stop (ms). Engines give a cancelled turn up to 20 s
 *  before killing it, so the last check lands past that grace. */
export const STOP_RECONCILE_DELAYS_MS = [1500, 5000, 12000, 25000] as const;
