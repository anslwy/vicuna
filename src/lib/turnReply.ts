/** A channel's replies belong to its originating turn, even before startup
 * returns the id or after the session's current-turn map changes. Never fall
 * back to another turn: request ids are only unique within one turn. */
export function createTurnReplyRoute() {
  let bind!: (id: string | null) => void;
  const origin = new Promise<string | null>((resolve) => { bind = resolve; });
  return {
    bind,
    async send<R>(askId: string, answer: string, deliver: (turnId: string, askId: string, answer: string, record?: R) => Promise<void>, record?: R) {
      const turnId = await origin;
      if (!turnId) throw new Error("The originating Work turn failed to start");
      if (record === undefined) await deliver(turnId, askId, answer);
      else await deliver(turnId, askId, answer, record);
    }
  };
}
