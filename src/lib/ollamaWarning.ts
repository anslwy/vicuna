/** Whether a user-triggered Ollama selection/turn needs an unavailable warning. */
export function shouldWarnOllamaUnavailable(isOllamaModel: boolean, reachable: boolean): boolean {
  return isOllamaModel && !reachable;
}
