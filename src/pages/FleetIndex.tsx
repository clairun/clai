import React, { useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { listWorkspaces, createWorkspace } from '../workspace/client';
import { errText, num } from '../fleet/workspaceStatus';
import styles from './FleetIndex.module.css';

/**
 * Index content for `/fleet`. The user chose "auto-select most-recent
 * workspace", so on mount we resolve the most-recently-updated workspace
 * and redirect to it. When there are no workspaces yet, we render an
 * empty state with a create affordance instead.
 *
 * The redirect uses `replace` so the back button doesn't bounce between
 * `/fleet` and the workspace.
 */
const FleetIndex = () => {
  const navigate = useNavigate();
  const [state, setState] = useState<'loading' | 'empty' | 'error'>('loading');
  const [error, setError] = useState('');
  const [creating, setCreating] = useState(false);

  useEffect(() => {
    let cancelled = false;
    listWorkspaces()
      .then((all) => {
        if (cancelled) return;
        const list = all || [];
        if (list.length === 0) {
          setState('empty');
          return;
        }
        const mostRecent = list.reduce((best, ws) =>
          num(ws.updatedAt) > num(best.updatedAt) ? ws : best,
        );
        navigate(`/workspace/${mostRecent.id}`, { replace: true });
      })
      .catch((err) => {
        if (cancelled) return;
        setError(errText(err, 'Failed to load workspaces.'));
        setState('error');
      });
    return () => {
      cancelled = true;
    };
  }, [navigate]);

  const handleCreate = async () => {
    if (creating) return;
    setCreating(true);
    try {
      const id = await createWorkspace();
      navigate(`/workspace/${id}`, { replace: true });
    } catch (err) {
      setError(errText(err, 'Failed to create workspace.'));
      setCreating(false);
    }
  };

  if (state === 'loading') {
    return <div className={styles.center}>Loading workspaces…</div>;
  }

  if (state === 'error') {
    return <div className={styles.center}>{error}</div>;
  }

  return (
    <div className={styles.center}>
      <h2 className={styles.title}>No workspaces yet</h2>
      <p className={styles.text}>
        Create your first workspace to start working with an agent.
      </p>
      <button
        type="button"
        className={styles.createButton}
        onClick={handleCreate}
        disabled={creating}
      >
        {creating ? 'Creating…' : '＋ New workspace'}
      </button>
    </div>
  );
};

export default FleetIndex;
