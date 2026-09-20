import { useMemo } from 'react';
import type { WorkspaceAgentResponse } from '../../generated/bindings';

const EMPTY_ROSTER: readonly WorkspaceAgentResponse[] = [];

/**
 * The crew, keyed on the faces it draws.
 *
 * The workspace details object is rebuilt from scratch on every 5s poll, so
 * `details.assignedAgents` is a new array each time even when nobody joined,
 * left or changed their face. Handing that array to the chat would repaint
 * every agent face and every task card four times a minute for nothing.
 *
 * This returns the same reference until something a face actually depends on
 * changes — identity, Main-ness, name, avatar seed and generator. Anything an
 * avatar starts reading must be added to the key, which is why the key is
 * built here, once, instead of at each call site.
 */
const rosterKey = (agents: readonly WorkspaceAgentResponse[] | null | undefined): string =>
  (agents || [])
    .map((agent) =>
      [
        agent.id,
        agent.isDefault ? '1' : '0',
        agent.displayName,
        agent.avatar?.seed ?? '',
        agent.avatar?.generatorVersion ?? '',
      ].join(':')
    )
    .join('|');

export const useStableRoster = (
  agents: readonly WorkspaceAgentResponse[] | null | undefined
): readonly WorkspaceAgentResponse[] => {
  const key = rosterKey(agents);
  return useMemo(
    () => agents || EMPTY_ROSTER,
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed on the crew's *content*: a poll that rebuilt the same faces must not hand the chat a new reference.
    [key]
  );
};
