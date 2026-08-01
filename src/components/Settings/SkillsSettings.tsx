import React, { useEffect, useMemo, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import {
  addSkillSource,
  deleteSkillSource,
  getSkillsCatalog,
  refreshSkillSource,
  setSkillSourceEnabled,
} from '../../api/client';
import type {
  SkillDefinition,
  SkillSourceDiagnostic,
  SkillSourceResponse,
} from '../../generated/bindings';
import styles from './SkillsSettings.module.css';

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

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

const ChevronIcon = ({ open }: { open: boolean }) => (
  <svg
    className={`${styles.chevron} ${open ? styles.chevronOpen : ''}`}
    width="12"
    height="12"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2.5"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <polyline points="9 18 15 12 9 6" />
  </svg>
);

const sourcePath = (source: SkillSourceResponse | undefined): string => {
  if (!source?.source) return '';
  if (source.source.kind === 'local') return source.source.path || '';
  // Binding field is snake_case (`local_path`); the .jsx read `localPath`.
  return source.source.uri || source.source.local_path || '';
};

const basename = (path: string): string => {
  const normalized = path.replace(/\\/g, '/').replace(/\/+$/, '');
  return normalized.split('/').pop() || path;
};

const repoNameFromUri = (uri: string): string => basename(uri).replace(/\.git$/, '') || 'Skills repo';
const isBundledSource = (source: SkillSourceResponse | undefined): boolean =>
  source?.managedKind === 'bundled';
const managedLabel = (source: SkillSourceResponse | undefined): string | null => {
  if (isBundledSource(source)) return 'Default';
  return null;
};

const SkillsSettings = () => {
  const [sources, setSources] = useState<SkillSourceResponse[]>([]);
  const [skills, setSkills] = useState<SkillDefinition[]>([]);
  const [diagnostics, setDiagnostics] = useState<SkillSourceDiagnostic[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [sourceKind, setSourceKind] = useState<'local' | 'git'>('local');
  const [name, setName] = useState('');
  const [path, setPath] = useState('');
  const [uri, setUri] = useState('');
  const [reference, setReference] = useState('');
  const [saving, setSaving] = useState(false);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [refreshingId, setRefreshingId] = useState<string | null>(null);
  const [togglingId, setTogglingId] = useState<string | null>(null);
  const [showAddForm, setShowAddForm] = useState(false);
  const [query, setQuery] = useState('');
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const skillsBySource = useMemo(() => {
    const grouped = new Map<string, SkillDefinition[]>();
    for (const skill of skills) {
      const bucket = grouped.get(skill.sourceId);
      if (bucket) bucket.push(skill);
      else grouped.set(skill.sourceId, [skill]);
    }
    for (const bucket of grouped.values()) {
      bucket.sort((left, right) => left.name.localeCompare(right.name));
    }
    return grouped;
  }, [skills]);

  const trimmedQuery = query.trim().toLowerCase();

  // A source stays visible if its own name matches, or if any of its skills
  // do; in the latter case only the matching skills are listed.
  const visibleSources = useMemo(() => {
    return sources
      .map((source) => {
        const own = skillsBySource.get(source.id) || [];
        if (!trimmedQuery) return { source, skills: own, total: own.length };
        const sourceMatches = source.name.toLowerCase().includes(trimmedQuery);
        const matching = own.filter(
          (skill) =>
            skill.name.toLowerCase().includes(trimmedQuery) ||
            skill.description.toLowerCase().includes(trimmedQuery)
        );
        if (!sourceMatches && matching.length === 0) return null;
        return { source, skills: sourceMatches ? own : matching, total: own.length };
      })
      .filter((entry): entry is { source: SkillSourceResponse; skills: SkillDefinition[]; total: number } => entry !== null);
  }, [sources, skillsBySource, trimmedQuery]);

  const diagnosticsBySource = useMemo(
    () => new Map(diagnostics.map((diagnostic) => [diagnostic.sourceId, diagnostic])),
    [diagnostics]
  );

  useEffect(() => {
    // eslint-disable-next-line react-hooks/immutability -- One-shot async bootstrap: loadCatalog is declared below with `const` so the linter cannot prove the closure value at effect registration; the function only reads the initial state via setSources/setSkills/setDiagnostics, so the TDZ is benign here.
    loadCatalog();
  }, []);

  const loadCatalog = async () => {
    setLoading(true);
    setError(null);
    try {
      const catalog = (await getSkillsCatalog()) as {
        sources?: SkillSourceResponse[];
        skills?: SkillDefinition[];
        diagnostics?: SkillSourceDiagnostic[];
      } | null;
      setSources(catalog?.sources || []);
      setSkills(catalog?.skills || []);
      setDiagnostics(catalog?.diagnostics || []);
    } catch (loadError) {
      console.error('[SkillsSettings] Failed to load skills catalog:', loadError);
      setError(errText(loadError, 'Failed to load skill catalog.'));
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

  const handleAddSource = async (event: React.FormEvent) => {
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
      setShowAddForm(false);
      await loadCatalog();
    } catch (saveError) {
      console.error('[SkillsSettings] Failed to add skill source:', saveError);
      setError(errText(saveError, 'Failed to add skill source.'));
    } finally {
      setSaving(false);
    }
  };

  const handleRefreshSource = async (sourceId: string) => {
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
      setError(errText(refreshError, 'Failed to refresh skill source.'));
    } finally {
      setRefreshingId(null);
    }
  };

  const handleToggleSource = async (source: SkillSourceResponse) => {
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
      setError(errText(toggleError, 'Failed to update skill source.'));
    } finally {
      setTogglingId(null);
    }
  };

  const handleDeleteSource = async (sourceId: string) => {
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
      setError(errText(deleteError, 'Failed to delete skill source.'));
    } finally {
      setDeletingId(null);
    }
  };

  const toggleCollapsed = (sourceId: string) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(sourceId)) next.delete(sourceId);
      else next.add(sourceId);
      return next;
    });
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
        <button
          type="button"
          className={styles.addButton}
          onClick={() => setShowAddForm((current) => !current)}
          aria-expanded={showAddForm}
        >
          <PlusIcon />
          <span>{showAddForm ? 'Cancel' : 'Add source'}</span>
        </button>
      </div>

      {error && <div className={styles.errorBanner}>{error}</div>}

      {showAddForm && (
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
      )}

      {loading ? (
        <div className={styles.loadingState}>Loading skills...</div>
      ) : sources.length === 0 ? (
        <div className={styles.emptyState}>
          No skill sources configured. Add one to discover SKILL.md files.
        </div>
      ) : (
        <section className={styles.section}>
          <div className={styles.sectionHeader}>
            <h4 className={styles.sectionTitle}>Sources</h4>
            <span className={styles.count}>{sources.length}</span>
            <span className={styles.sectionMeta}>
              {skills.length} skill{skills.length === 1 ? '' : 's'}
            </span>
            <input
              type="search"
              className={styles.searchInput}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search skills…"
              aria-label="Search skills"
            />
          </div>

          {visibleSources.length === 0 ? (
            <div className={styles.emptyState}>No skills match “{query.trim()}”.</div>
          ) : (
            <div className={styles.sourceList}>
              {visibleSources.map(({ source, skills: sourceSkills, total }) => {
                // While searching, matching sources stay open — collapsing one
                // the user just searched into would hide the answer.
                const open = Boolean(trimmedQuery) || !collapsed.has(source.id);
                const diagnostic = diagnosticsBySource.get(source.id);
                const bodyId = `skill-source-${source.id}`;
                return (
                  <div key={source.id} className={styles.sourceCard}>
                    <div className={styles.sourceTopRow}>
                      <button
                        type="button"
                        className={styles.sourceToggle}
                        onClick={() => toggleCollapsed(source.id)}
                        aria-expanded={open}
                        aria-controls={bodyId}
                        disabled={Boolean(trimmedQuery)}
                      >
                        <ChevronIcon open={open} />
                        <span className={styles.sourceName}>{source.name}</span>
                        <span className={`${styles.statusBadge} ${source.enabled ? styles.enabled : styles.disabled}`}>
                          {source.enabled ? 'Enabled' : 'Disabled'}
                        </span>
                        <span className={styles.kindBadge}>{source.source?.kind || 'local'}</span>
                        {managedLabel(source) && (
                          <span className={isBundledSource(source) ? styles.bundledBadge : styles.personalBadge}>
                            {managedLabel(source)}
                          </span>
                        )}
                        <span className={styles.sourceCountText}>
                          {total} skill{total === 1 ? '' : 's'}
                        </span>
                      </button>
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
                        {!source.managedKind && (
                          <button
                            type="button"
                            className={styles.deleteButton}
                            onClick={() => handleDeleteSource(source.id)}
                            disabled={deletingId === source.id}
                          >
                            {deletingId === source.id ? 'Deleting...' : 'Delete'}
                          </button>
                        )}
                      </div>
                    </div>

                    {diagnostic?.message && (
                      <div className={`${styles.sourceDiagnostic} ${diagnostic.ok ? styles.sourceDiagnosticMuted : styles.sourceDiagnosticError}`}>
                        {diagnostic.message}
                      </div>
                    )}

                    {open && (
                      <div id={bodyId} className={styles.sourceBody}>
                        <div className={styles.sourcePath}>{sourcePath(source)}</div>
                        {isBundledSource(source) && (
                          <div className={styles.sourceMeta}>
                            Read-only. Refresh pulls updates from the CLAI skills repository.
                          </div>
                        )}
                        {source.source?.kind === 'git' && source.source?.reference && (
                          <div className={styles.sourceMeta}>Ref: {source.source.reference}</div>
                        )}
                        {sourceSkills.length === 0 ? (
                          <div className={styles.sourceMeta}>No SKILL.md files discovered.</div>
                        ) : (
                          <ul className={styles.skillList}>
                            {sourceSkills.map((skill) => (
                              <li key={skill.id} className={styles.skillRow}>
                                <span className={styles.skillName}>{skill.name}</span>
                                {skill.description && (
                                  <p className={styles.skillDescription}>{skill.description}</p>
                                )}
                                <code className={styles.skillPath}>{skill.sourcePath}</code>
                              </li>
                            ))}
                          </ul>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </section>
      )}
    </div>
  );
};

export default SkillsSettings;
