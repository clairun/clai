import { useRef, useState } from 'react';

const message = (e: unknown) =>
  typeof e === 'string' ? e : e instanceof Error ? e.message : 'Failed to submit decision';

// How the three submit commands refuse a request no longer in their queue
// (commands/permissions.rs, path_grants.rs, network_access.rs).
const isAlreadyResolved = (text: string) => text.includes('(already resolved or cleared)');

/**
 * Sends a request card's decision once. A refused or failed decision leaves
 * the request pending, so the card can be answered again. A request that
 * already left the queue (answered in another view, or its run ended,
 * and the resolved event was missed) is `resolved`: there is nothing left
 * to answer.
 */
export const useDecisionSender = <D>(submit: (decision: D) => Promise<void>, onDecided?: () => void) => {
  const [isSubmitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [resolved, setResolved] = useState(false);
  // Latched once the decision is sent: a click that lands before the
  // buttons disable must not send a second decision.
  const sent = useRef(false);

  const send = async (decision: D) => {
    if (sent.current) return;
    sent.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await submit(decision);
      onDecided?.();
    } catch (e) {
      const text = message(e);
      if (isAlreadyResolved(text)) {
        setResolved(true);
      } else {
        sent.current = false;
        setError(text);
      }
    } finally {
      setSubmitting(false);
    }
  };

  return { send, isSubmitting, error, resolved };
};
