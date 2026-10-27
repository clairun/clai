// Mirrors the backend `is_usage_limit_error` classifier (local_agent.rs).
// Used for presentation in Workspace and Fleet.
export const isUsageLimitError = (message: string): boolean => {
  const m = message.toLowerCase();
  return (
    m.includes('usage limit') ||
    m.includes('session limit') ||
    m.includes('rate limit') ||
    m.includes('rate_limit') ||
    m.includes('quota') ||
    (m.includes("you've hit your") && m.includes('limit'))
  );
};
