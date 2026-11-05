/** Saving was refused because the agent changed after the form loaded it. */
export class StaleAgentError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'StaleAgentError';
  }
}
