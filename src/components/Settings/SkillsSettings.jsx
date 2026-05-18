import React, { useEffect, useMemo, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import {
  addSkillSource,
  deleteSkillSource,
  getSkillsCatalog,
  refreshSkillSource,
  setSkillSourceEnabled,
} from '../../api/client';
import styles from './SkillsSettings.module.css';

const PlusIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="12" y1="5" x2="12" y2="19" />
    <line x1="5" y1="12" x2="19" y2="12" />
  </svg>
);

const FolderIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" />
  </svg>
);

const sourcePath = (source) => {
  if (!source?.source) return '';
  if (source.source.kind === 'local') return source.source.path || '';
  return source.source.uri || source.source.localPath || '';
};

const basename = (path) => {
  const normalized = path.replace(/\\/g, '/').replace(/\/+$/, '');
  return normalized.split('/').pop() || path;
};

const repoNameFromUri = (uri) => basename(uri).replace(/\.git$/, '') || 'Skills repo';

const SkillsSettings = () => {
  const [sources, setSources] = useState([]);
  const [skills, setSkills] = useState([]);
  const [diagnostics, setDiagnostics] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [sourceKind, setSourceKind] = useState('local');
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [uri, setUri] = useState('');
  const [reference, setReference] = useState('');
  const [saving, setSaving] = useState(false);
  const [deletingId, setDeletingId] = useState(null);
  const [refreshingId, setRefreshingId] = useState(null);
  const [togglingId, setTogglingId] = useState(null);

  const skillsBySource = useMemo(() => {
    const counts = new Map();
    for (const skill of skills) {
      counts.set(skill.sourceId, (counts.get(skill.sourceId) || 0) + 1);
    }
    return counts;
  }, [skills]);

  const diagnosticsBySource = useMemo(
    () => new Map(diagnostics.map((diagnostic) => [diagnostic.sourceId, diagnostic])),
    [diagnostics]
  );

  useEffect(() => {
    loadCatalog();
  }, []);

  const loadCatalog = async () => {
    setLoading(true);
    setError(null);
    try {
      const catalog = await getSkillsCatalog();
      setSources(catalog?.sources || []);
      setSkills(catalog?.skills || []);
      setDiagnostics(catalog?.diagnostics || []);
    } catch (loadError) {
      console.error('[SkillsSettings] Failed to load skills catalog:', loadError);
      setError(loadError?.message || 'Failed to load skill catalog.');
    } finally {
      setLoading(false);
    }
  };

  const handlePickPath = async () => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: 'Select skill source directory',
    });
    if (!selected) {
      return;
    }
    const selectedPath = Array.isArray(selected) ? selected[0] : selected;
    if (!selectedPath) {
      return;
    }
    setPath(selectedPath);
    setName((current) => current.trim() || basename(selectedPath));
  };

  const handleAddSource = async (event) => {
    event.preventDefault();
    const trimmedName = name.trim();
    const trimmedPath = path.trim();
    const trimmedUri = uri.trim();
    const trimmedReference = reference.trim();
    if (!trimmedName || saving) {
      return;
    }
    if (sourceKind === 'local' && !trimmedPath) {
      return;
    }
    if (sourceKind === 'git' && !trimmedUri) {
      return;
    }

    setSaving(true);
    setError(null);
    try {
      await addSkillSource({
        kind: sourceKind,
        name: trimmedName,
        path: sourceKind === 'local' ? trimmedPath : undefined,
        uri: sourceKind === 'git' ? trimmedUri : undefined,
        reference: sourceKind === 'git' && trimmedReference ? trimmedReference : undefined,
      });
      setName('');
      setPath('');
      setUri('');
      setReference('');
      await loadCatalog();
    } catch (saveError) {
      console.error('[SkillsSettings] Failed to add skill source:', saveError);
      setError(saveError?.message || 'Failed to add skill source.');
    } finally {
      setSaving(false);
    }
  };

  const handleRefreshSource = async (sourceId) => {
    if (refreshingId) {
      return;
    }
    setRefreshingId(sourceId);
    setError(null);
    try {
      await refreshSkillSource(sourceId);
      await loadCatalog();
    } catch (refreshError) {
      console.error('[SkillsSettings] Failed to refresh skill source:', refreshError);
      setError(refreshError?.message || 'Failed to refresh skill source.');
    } finally {
      setRefreshingId(null);
    }
  };

  const handleToggleSource = async (source) => {
    if (togglingId) {
      return;
    }
    setTogglingId(source.id);
    setError(null);
    try {
      await setSkillSourceEnabled(source.id, !source.enabled);
      await loadCatalog();
    } catch (toggleError) {
      console.error('[SkillsSettings] Failed to update skill source:', toggleError);
      setError(toggleError?.message || 'Failed to update skill source.');
    } finally {
      setTogglingId(null);
    }
  };

  const handleDeleteSource = async (sourceId) => {
    if (deletingId) {
      return;
    }
    setDeletingId(sourceId);
    setError(null);
    try {
      await deleteSkillSource(sourceId);
      await loadCatalog();
    } catch (deleteError) {
      console.error('[SkillsSettings] Failed to delete skill source:', deleteError);
      setError(deleteError?.message || 'Failed to delete skill source.');
    } finally {
      setDeletingId(null);
    }
  };

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <div className={styles.headerText}>
          <h3 className={styles.title}>Skills</h3>
          <p className={styles.description}>
            Register skill repositories and assign discovered skills to agents.
          </p>
        </div>
      </div>

      {error && <div className={styles.errorBanner}>{error}</div>}

      <form className={styles.addSourceForm} onSubmit={handleAddSource}>
        <div className={styles.sourceTypeControl}>
          <button
            type="button"
            className={`${styles.sourceTypeButton} ${sourceKind === 'local' ? styles.sourceTypeButtonActive : ''}`}
            onClick={() => setSourceKind('local')}
            disabled={saving}
          >
            Local
          </button>
          <button
            type="button"
            className={`${styles.sourceTypeButton} ${sourceKind === 'git' ? styles.sourceTypeButtonActive : ''}`}
            onClick={() => {
              setSourceKind('git');
              setName((current) => current.trim() || (uri.trim() ? repoNameFromUri(uri.trim()) : ''));
            }}
            disabled={saving}
          >
            Git
          </button>
        </div>
        <div className={styles.formGrid}>
          <label className={styles.field}>
            <span className={styles.label}>Name</span>
            <input
              className={styles.input}
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="Company skills"
              disabled={saving}
            />
          </label>
          {sourceKind === 'local' ? (
            <label className={styles.field}>
              <span className={styles.label}>Directory</span>
              <div className={styles.pathRow}>
                <input
                  className={styles.input}
                  value={path}
                  onChange={(event) => setPath(event.target.value)}
                  placeholder="/path/to/skills"
                  disabled={saving}
                />
                <button
                  type="button"
                  className={styles.secondaryButton}
                  onClick={handlePickPath}
                  disabled={saving}
                  title="Choose directory"
                >
                  <FolderIcon />
                </button>
              </div>
            </label>
          ) : (
            <>
              <label className={styles.field}>
                <span className={styles.label}>Repository URL</span>
                <input
                  className={styles.input}
                  value={uri}
                  onChange={(event) => {
                    const nextUri = event.target.value;
                    setUri(nextUri);
                    setName((current) => current.trim() || repoNameFromUri(nextUri));
                  }}
                  placeholder="https://github.com/company/skills.git"
                  disabled={saving}
                />
              </label>
              <label className={styles.field}>
                <span className={styles.label}>Ref</span>
                <input
                  className={styles.input}
                  value={reference}
                  onChange={(event) => setReference(event.target.value)}
                  placeholder="main, tag, or commit"
                  disabled={saving}
                />
              </label>
            </>
          )}
        </div>
        <button
          className={styles.addButton}
          type="submit"
          disabled={saving || !name.trim() || (sourceKind === 'local' ? !path.trim() : !uri.trim())}
        >
          <PlusIcon />
          <span>{saving ? 'Adding...' : 'Add Source'}</span>
        </button>
      </form>

      {loading ? (
        <div className={styles.loadingState}>Loading skills...</div>
      ) : (
        <>
          <section className={styles.section}>
            <div className={styles.sectionHeader}>
              <h4 className={styles.sectionTitle}>Sources</h4>
              <span className={styles.count}>{sources.length}</span>
            </div>
            {sources.length === 0 ? (
              <div className={styles.emptyState}>No skill sources configured.</div>
            ) : (
              <div className={styles.sourceList}>
                {sources.map((source) => (
                  <div key={source.id} className={styles.sourceCard}>
                    <div className={styles.sourceMain}>
                      <div className={styles.sourceNameRow}>
                        <span className={styles.sourceName}>{source.name}</span>
                        <span className={`${styles.statusBadge} ${source.enabled ? styles.enabled : styles.disabled}`}>
                          {source.enabled ? 'Enabled' : 'Disabled'}
                        </span>
                        <span className={styles.kindBadge}>{source.source?.kind || 'local'}</span>
                      </div>
                      <div className={styles.sourcePath}>{sourcePath(source)}</div>
                      {source.source?.kind === 'git' && source.source?.reference && (
                        <div className={styles.sourceMeta}>Ref: {source.source.reference}</div>
                      )}
                      {source.source?.kind === 'git' && source.source?.localPath && (
                        <div className={styles.sourceMeta}>Cache: {source.source.localPath}</div>
                      )}
                      <div className={styles.sourceMeta}>
                        {skillsBySource.get(source.id) || 0} skill{(skillsBySource.get(source.id) || 0) === 1 ? '' : 's'}
                      </div>
                      {diagnosticsBySource.get(source.id)?.message && (
                        <div className={`${styles.sourceDiagnostic} ${diagnosticsBySource.get(source.id)?.ok ? styles.sourceDiagnosticMuted : styles.sourceDiagnosticError}`}>
                          {diagnosticsBySource.get(source.id).message}
                        </div>
                      )}
                    </div>
                    <div className={styles.sourceActions}>
                      {source.source?.kind === 'git' && (
                        <button
                          type="button"
                          className={styles.actionButton}
                          onClick={() => handleRefreshSource(source.id)}
                          disabled={refreshingId === source.id}
                        >
                          {refreshingId === source.id ? 'Refreshing...' : 'Refresh'}
                        </button>
                      )}
                      <button
                        type="button"
                        className={styles.actionButton}
                        onClick={() => handleToggleSource(source)}
                        disabled={togglingId === source.id}
                      >
                        {source.enabled ? 'Disable' : 'Enable'}
                      </button>
                      <button
                        type="button"
                        className={styles.deleteButton}
                        onClick={() => handleDeleteSource(source.id)}
                        disabled={deletingId === source.id}
                      >
                        {deletingId === source.id ? 'Deleting...' : 'Delete'}
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>

          <section className={styles.section}>
            <div className={styles.sectionHeader}>
              <h4 className={styles.sectionTitle}>Discovered Skills</h4>
              <span className={styles.count}>{skills.length}</span>
            </div>
            {skills.length === 0 ? (
              <div className={styles.emptyState}>No SKILL.md files discovered.</div>
            ) : (
              <div className={styles.skillList}>
                {skills.map((skill) => (
                  <div key={skill.id} className={styles.skillCard}>
                    <div className={styles.skillHeader}>
                      <span className={styles.skillName}>{skill.name}</span>
                      <span className={styles.sourceBadge}>{skill.sourceName}</span>
                    </div>
                    {skill.description && (
                      <p className={styles.skillDescription}>{skill.description}</p>
                    )}
                    <code className={styles.skillPath}>{skill.sourcePath}</code>
                  </div>
                ))}
              </div>
            )}
          </section>
        </>
      )}
    </div>
  );
};

export default SkillsSettings;
