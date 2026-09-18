/**
 * The shared agent library.
 *
 * A definition here is behavior without a home: instructions, skills,
 * providers, MCP selection and execution policy, and nothing workspace-shaped —
 * no history, no schedule, no files. It starts working when a workspace puts it
 * on its crew, and an edit made here reaches every workspace that did, on their
 * next turn. What stays local to each workspace is its Main agent, edited in
 * that workspace's settings.
 *
 * Two screens in one pane, never both: the gallery of cards, or the editor
 * for one agent (or a new one).
 */

import { useCallback, useEffect, useState } from 'react';
import {
  getMcpServers,
  getSkills,
  listAgentDefinitions,
  workspaceAgentDefaultExecution,
  type AgentDefinitionDetail,
} from '../../api/client';
import { assistantClient } from '../../assistant';
import AgentGallery from '../Agents/AgentGallery';
import AgentEditor from './AgentEditor';
import type { AgentFormDeps } from './AgentBehaviorForm';
import styles from './AgentLibrarySettings.module.css';

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

type View =
  | { kind: 'gallery'; focusId: string | null }
  | { kind: 'edit'; id: string }
  | { kind: 'create' };

const AgentLibrarySettings = () => {
  const [definitions, setDefinitions] = useState<AgentDefinitionDetail[]>([]);
  const [view, setView] = useState<View>({ kind: 'gallery', focusId: null });
  const [deps, setDeps] = useState<AgentFormDeps>({
    mcpServers: [],
    skills: [],
    providerConnections: [],
    defaultExecution: undefined,
  });
  const [error, setError] = useState<string | null>(null);

  /** Re-reads the library; `false` when it could not, with the error shown. */
  const reload = useCallback(async (): Promise<boolean> => {
    try {
      setDefinitions(await listAgentDefinitions());
      setError(null);
      return true;
    } catch (err) {
      setError(errText(err, 'Failed to load the agent library.'));
      return false;
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [servers, skills, connections, defaults] = await Promise.allSettled([
        getMcpServers(),
        getSkills(),
        assistantClient.listProviderConnections(),
        workspaceAgentDefaultExecution(),
      ]);
      if (cancelled) return;
      setDeps({
        mcpServers: servers.status === 'fulfilled' ? servers.value || [] : [],
        skills: skills.status === 'fulfilled' ? skills.value || [] : [],
        providerConnections: connections.status === 'fulfilled' ? connections.value || [] : [],
        defaultExecution: defaults.status === 'fulfilled' ? defaults.value || null : null,
      } as AgentFormDeps);
      await reload();
    })();
    return () => {
      cancelled = true;
    };
  }, [reload]);

  const backToGallery = useCallback(() => setView({ kind: 'gallery', focusId: null }), []);

  // A create lands back in the gallery with the new card focused; an edit
  // stays open on the reloaded agent. If the reload failed the editor would
  // keep a stale revision and every later save would be refused, so it goes
  // back to the gallery too, where the error and Retry are.
  const handleSaved = useCallback(
    async (id: string, created: boolean) => {
      const fresh = await reload();
      if (created || !fresh) setView({ kind: 'gallery', focusId: id });
      else setView({ kind: 'edit', id });
    },
    [reload]
  );

  const editing =
    view.kind === 'edit' ? definitions.find((definition) => definition.id === view.id) : undefined;

  return (
    <div className={styles.container}>
      {error && (
        <div className={styles.errorBanner} role="alert">
          <span>{error}</span>
          <button type="button" className={styles.retry} onClick={() => void reload()}>
            Retry
          </button>
        </div>
      )}

      {view.kind === 'gallery' && (
        <AgentGallery
          definitions={definitions}
          focusId={view.focusId}
          onOpen={(id) => setView({ kind: 'edit', id })}
          onCreate={() => setView({ kind: 'create' })}
        />
      )}

      {view.kind === 'create' && (
        <AgentEditor key="create" deps={deps} onBack={backToGallery} onSaved={handleSaved} />
      )}

      {view.kind === 'edit' &&
        (editing ? (
          // Remount per agent: the editor and the form mirror their agent into
          // local state at mount time.
          <AgentEditor
            key={editing.id}
            definition={editing}
            deps={deps}
            onBack={backToGallery}
            onSaved={handleSaved}
          />
        ) : (
          <div className={styles.missing}>
            <p>This agent is no longer in the library.</p>
            <button type="button" onClick={backToGallery}>
              Back to agents
            </button>
          </div>
        ))}
    </div>
  );
};

export default AgentLibrarySettings;
