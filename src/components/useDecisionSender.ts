import { useRef, useState } from 'react';

const message = (e: unknown) =>
  typeof e === 'string' ? e : e instanceof Error ? e.message : 'Failed to submit decision';

/**
 * Sends a request card's decision once. A refused or failed decision leaves
 * the request pending, so the card can be answered again.
 */
export const useDecisionSender = <D>(submit: (decision: D) => Promise<void>, onDecided?: () => void) => {
  const [isSubmitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
      sent.current = false;
      setError(message(e));
    } finally {
      setSubmitting(false);
    }
  };

  return { send, isSubmitting, error };
};
