import { Fragment, useCallback, useEffect, useReducer, useRef, type ReactNode, type Ref } from 'react';
import { listen } from '@tauri-apps/api/event';

interface PendingRequest {
  requestId: string;
  workspaceId: string | null;
}

/** Where one kind of request comes from: the backend's pending list and its two events. */
export interface PendingRequestSource<T extends PendingRequest> {
  list: (workspaceId: string) => Promise<T[]>;
  /** Carries a new request, or one asked again after a failed save. */
  requestEvent: string;
  /**
   * Carries `{ requestId }` once the request leaves the backend queue: a
   * decision here or in another view, a cancelled or ended run, or a request
   * that superseded it.
   */
  resolvedEvent: string;
  /** The event payload is a whole request of this kind. */
  isRequest: (payload: T) => boolean;
}

type Action<T> = { type: 'reset' } | { type: 'add'; incoming: T[] } | { type: 'remove'; requestId: string };

const pendingReducer = <T extends PendingRequest>(requests: T[], action: Action<T>): T[] => {
  switch (action.type) {
    case 'reset':
      return requests.length === 0 ? requests : [];
    case 'add': {
      const known = new Set(requests.map((req) => req.requestId));
      const additions = action.incoming.filter((req) => !known.has(req.requestId));
      return additions.length === 0 ? requests : [...requests, ...additions];
    }
    case 'remove':
      return requests.some((req) => req.requestId === action.requestId)
        ? requests.filter((req) => req.requestId !== action.requestId)
        : requests;
  }
};

/**
 * A workspace's pending requests of one kind: seeded from the backend's list,
 * which catches requests made before this view mounted, then kept by the
 * request and resolved events. `source` must be a module constant.
 */
export const usePendingRequests = <T extends PendingRequest>(
  workspaceId: string | null,
  source: PendingRequestSource<T>,
) => {
  const [requests, dispatch] = useReducer(pendingReducer<T>, []);

  useEffect(() => {
    // The Workspace page does not remount when it switches workspaces, so
    // the previous workspace's requests are dropped before seeding again.
    dispatch({ type: 'reset' });
    if (!workspaceId) return undefined;

    let cancelled = false;
    source
      .list(workspaceId)
      .then((pending) => {
        if (!cancelled && Array.isArray(pending)) dispatch({ type: 'add', incoming: pending });
      })
      .catch(() => {
        // Non-fatal: the events still bring new requests.
      });

    // Tauri's unlisten is async, so after a switch the previous workspace's
    // listener can still fire, and its own workspaceId passes the filter;
    // `cancelled` stops it synchronously.
    const unlistenRequest = listen<T>(source.requestEvent, (event) => {
      if (cancelled) return;
      const req = event.payload;
      if (!req?.requestId || req.workspaceId !== workspaceId || !source.isRequest(req)) return;
      dispatch({ type: 'add', incoming: [req] });
    });
    // requestId is unique across workspaces, so this needs no filter.
    const unlistenResolved = listen<{ requestId?: string }>(source.resolvedEvent, (event) => {
      if (cancelled) return;
      const requestId = event.payload?.requestId;
      if (requestId) dispatch({ type: 'remove', requestId });
    });

    return () => {
      cancelled = true;
      unlistenRequest.then((unlisten) => unlisten()).catch(() => {});
      unlistenResolved.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [workspaceId, source]);

  const dismiss = useCallback((requestId: string) => dispatch({ type: 'remove', requestId }), []);

  return { requests, dismiss };
};

/** What the stack hands each card it draws. */
export interface StackedCard {
  ref: Ref<HTMLElement> | null;
  /** The card's decision was accepted: drop it. */
  onDecided: (requestId: string) => void;
}

interface PendingRequestStackProps<T extends PendingRequest> {
  workspaceId: string | null;
  source: PendingRequestSource<T>;
  className?: string;
  renderCard: (req: T, card: StackedCard) => ReactNode;
}

/**
 * A workspace's pending requests of one kind, one card each, beside the chat
 * that raised them.
 */
export const PendingRequestStack = <T extends PendingRequest>({
  workspaceId,
  source,
  className,
  renderCard,
}: PendingRequestStackProps<T>) => {
  const { requests, dismiss } = usePendingRequests(workspaceId, source);
  const firstCardRef = useRef<HTMLElement | null>(null);
  const previousCountRef = useRef(0);

  // Scroll into view on the first arrival only, not on every later one.
  useEffect(() => {
    const previous = previousCountRef.current;
    previousCountRef.current = requests.length;
    if (previous === 0 && requests.length > 0) {
      firstCardRef.current?.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }
  }, [requests.length]);

  if (requests.length === 0) return null;

  return (
    <div className={className} aria-live="polite">
      {requests.map((req, index) => (
        <Fragment key={req.requestId}>
          {renderCard(req, { ref: index === 0 ? firstCardRef : null, onDecided: dismiss })}
        </Fragment>
      ))}
    </div>
  );
};
