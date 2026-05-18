/**
 * AgentFormModal Component
 *
 * Modal form for creating and editing agents.
 */

import React, { useState, useEffect, useCallback, useMemo } from 'react';
import ReactDOM from 'react-dom';
import IntervalSelect from './IntervalSelect';
import styles from './AgentFormModal.module.css';

const defaultExecution = () => ({
  filesystem: {
    extraPaths: [],
  },
  shell: {
    mode: 'off',
    allowedCommandPrefixes: [],
    blockedCommandPrefixes: ['rm', 'sudo', 'chmod', 'chown', 'dd', 'mkfs', 'mount', 'umount', 'shutdown', 'reboot'],
  },
  web: {
    enabled: false,
  },
});

const normalizeItems = (items = []) => items.map((item) => item.trim()).filter(Boolean);
const addUniqueItem = (items, value) => {
  const trimmed = value.trim();
  if (!trimmed || items.includes(trimmed)) {
    return items;
  }
  return [...items, trimmed];
};
const normalizePathGrants = (items = []) =>
  items
    .map((item) => ({
      path: item.path?.trim() || '',
      access: item.access || 'read_only',
    }))
    .filter((item) => item.path);

/**
 * Close icon
 */
const CloseIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="18" y1="6" x2="6" y2="18" />
    <line x1="6" y1="6" x2="18" y2="18" />
  </svg>
);

/**
 * Loading spinner
 */
const LoadingIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className={styles.spinner}>
    <circle cx="12" cy="12" r="10" opacity="0.25" />
    <path d="M12 2a10 10 0 0 1 10 10" />
  </svg>
);

const ListInputField = ({
  label,
  value,
  onChange,
  onAdd,
  items,
  onRemove,
  placeholder,
  disabled,
  hint,
  id,
  variant,
}) => (
  <div className={styles.field}>
    <label className={styles.label} htmlFor={id}>
      {label}
    </label>
    <input
      id={id}
      type="text"
      className={styles.input}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault();
          onAdd();
        }
      }}
      placeholder={placeholder}
      disabled={disabled}
    />
    {items.length > 0 && (
      <div className={styles.chipList}>
        {items.map((item) => (
          <span key={item} className={`${styles.chip} ${variant === 'danger' ? styles.chipDanger : ''}`}>
            <code>{item}</code>
            <button
              type="button"
              className={styles.chipRemove}
              onClick={() => onRemove(item)}
              disabled={disabled}
              aria-label={`Remove ${item}`}
            >
              ×
            </button>
          </span>
        ))}
      </div>
    )}
    {hint && <span className={styles.hint}>{hint}</span>}
  </div>
);


/**
 * AgentFormModal - Create/Edit automation form
 *
 * @param {Object} props
 * @param {boolean} props.isOpen - Whether modal is open
 * @param {Function} props.onClose - Callback when modal closes
 * @param {Function} props.onSubmit - Callback with form data
 * @param {Object} props.agent - Agent to edit (null for create)
 */
const AgentFormModal = ({ isOpen, onClose, onSubmit, agent, mcpServers = [], providerConnections = [], skills = [] }) => {
  const isEditing = !!agent;

  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [scheduleEnabled, setScheduleEnabled] = useState(true);
  const [intervalMinutes, setIntervalMinutes] = useState(30);
  const [selectedMcpServerIds, setSelectedMcpServerIds] = useState([]);
  const [selectedSkillIds, setSelectedSkillIds] = useState([]);
  const [providerConnectionIds, setProviderConnectionIds] = useState([]);
  const [providerConnectionDraft, setProviderConnectionDraft] = useState('');
  const [extraPathGrants, setExtraPathGrants] = useState([]);
  const [extraPathDraft, setExtraPathDraft] = useState('');
  const [extraPathAccess, setExtraPathAccess] = useState('read_only');
  const [shellMode, setShellMode] = useState('off');
  const [allowedCommands, setAllowedCommands] = useState([]);
  const [blockedCommands, setBlockedCommands] = useState(defaultExecution().shell.blockedCommandPrefixes);
  const [allowedCommandDraft, setAllowedCommandDraft] = useState('');
  const [blockedCommandDraft, setBlockedCommandDraft] = useState('');
  const [webEnabled, setWebEnabled] = useState(false);
  const [exposedTools, setExposedTools] = useState([]);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState(null);

  const enabledProviderConnections = useMemo(
    () => providerConnections.filter((connection) => connection.enabled),
    [providerConnections]
  );

  const availableProviderConnections = useMemo(
    () => enabledProviderConnections.filter((connection) => !providerConnectionIds.includes(connection.id)),
    [enabledProviderConnections, providerConnectionIds]
  );

  // Reset form when modal opens/closes or agent changes
  useEffect(() => {
    if (isOpen) {
      const execution = agent?.execution || defaultExecution();
      if (agent) {
        setName(agent.name || '');
        setDescription(agent.description || '');
        setScheduleEnabled(agent.scheduleEnabled !== false);
        setIntervalMinutes(agent.intervalMinutes || 30);
        setSelectedMcpServerIds(agent.selectedMcpServerIds || []);
        setSelectedSkillIds(agent.selectedSkillIds || []);
        setProviderConnectionIds(agent.providerConnectionIds || []);
        setProviderConnectionDraft('');
        setExtraPathGrants(normalizePathGrants(execution.filesystem?.extraPaths || []));
        setExtraPathDraft('');
        setExtraPathAccess('read_only');
        setShellMode(execution.shell?.mode || 'off');
        setAllowedCommands(normalizeItems(execution.shell?.allowedCommandPrefixes || []));
        setBlockedCommands(normalizeItems(execution.shell?.blockedCommandPrefixes || defaultExecution().shell.blockedCommandPrefixes));
        setAllowedCommandDraft('');
        setBlockedCommandDraft('');
        setWebEnabled(execution.web?.enabled || false);
        setExposedTools(agent.exposedTools || []);
      } else {
        setName('');
        setDescription('');
        setScheduleEnabled(true);
        setIntervalMinutes(30);
        setSelectedMcpServerIds([]);
        setSelectedSkillIds([]);
        setProviderConnectionIds([]);
        setProviderConnectionDraft('');
        setExtraPathGrants([]);
        setExtraPathDraft('');
        setExtraPathAccess('read_only');
        setShellMode('off');
        setAllowedCommands([]);
        setBlockedCommands(defaultExecution().shell.blockedCommandPrefixes);
        setAllowedCommandDraft('');
        setBlockedCommandDraft('');
        setWebEnabled(false);
        setExposedTools([]);
      }
      setError(null);
    }
  }, [isOpen, agent]);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    if (providerConnectionDraft && availableProviderConnections.some((connection) => connection.id === providerConnectionDraft)) {
      return;
    }

    setProviderConnectionDraft(availableProviderConnections[0]?.id || '');
  }, [availableProviderConnections, isOpen, providerConnectionDraft]);

  // Handle escape key
  useEffect(() => {
    const handleEscape = (e) => {
      if (e.key === 'Escape' && isOpen && !saving) {
        onClose();
      }
    };

    document.addEventListener('keydown', handleEscape);
    return () => document.removeEventListener('keydown', handleEscape);
  }, [isOpen, saving, onClose]);

  // Prevent body scroll when modal is open
  useEffect(() => {
    if (isOpen) {
      document.body.style.overflow = 'hidden';
    } else {
      document.body.style.overflow = '';
    }
    return () => {
      document.body.style.overflow = '';
    };
  }, [isOpen]);

  const handleOverlayClick = useCallback((e) => {
    if (e.target === e.currentTarget && !saving) {
      onClose();
    }
  }, [saving, onClose]);

  const handleSubmit = async (e) => {
    e.preventDefault();
    setError(null);

    // Validation
    const trimmedName = name.trim();
    if (!trimmedName) {
      setError('Agent name is required');
      return;
    }

    if (trimmedName.length > 100) {
      setError('Agent name must be 100 characters or less');
      return;
    }

    if (scheduleEnabled && (intervalMinutes < 1 || intervalMinutes > 1440)) {
      setError('Interval must be between 1 minute and 24 hours');
      return;
    }

    if (providerConnectionIds.length === 0) {
      setError('Select at least one provider connection');
      return;
    }

    setSaving(true);

    try {
      await onSubmit({
        name: trimmedName,
        description: description.trim(),
        scheduleEnabled,
        intervalMinutes: Number(intervalMinutes),
        selectedMcpServerIds,
        selectedSkillIds,
        providerConnectionIds,
        execution: {
          filesystem: {
            extraPaths: extraPathGrants,
          },
          shell: {
            mode: shellMode,
            allowedCommandPrefixes: allowedCommands,
            blockedCommandPrefixes: blockedCommands,
          },
          web: {
            enabled: webEnabled,
          },
        },
        exposedTools,
      });
    } catch (err) {
      console.error('[AgentFormModal] Submit error:', err);
      setError(err.message || 'Failed to save agent. Please try again.');
    } finally {
      setSaving(false);
    }
  };

  if (!isOpen) {
    return null;
  }

  return ReactDOM.createPortal(
    <div className={styles.overlay} onClick={handleOverlayClick}>
      <div className={styles.modal} onClick={(e) => e.stopPropagation()}>
        {/* Header */}
        <div className={styles.header}>
          <h2 className={styles.title}>
            {isEditing ? 'Edit Agent' : 'Create Agent'}
          </h2>
          <button
            className={styles.closeButton}
            onClick={onClose}
            disabled={saving}
            title="Close"
          >
            <CloseIcon />
          </button>
        </div>

        {/* Form */}
        <form className={styles.form} onSubmit={handleSubmit}>
          {error && (
            <div className={styles.errorBanner}>
              {error}
            </div>
          )}

          <div className={styles.field}>
            <label className={styles.label} htmlFor="agent-name">
              Name <span className={styles.required}>*</span>
            </label>
            <input
              id="agent-name"
              type="text"
              className={styles.input}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g., Security Monitor, Performance Analyzer"
              disabled={saving}
              maxLength={100}
              autoFocus
            />
            <span className={styles.hint}>
              A descriptive name for this agent
            </span>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="agent-description">
              Description
            </label>
            <textarea
              id="agent-description"
              className={styles.textarea}
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Describe what this agent should focus on, what to look for, and how to report findings..."
              disabled={saving}
              rows={6}
            />
            <span className={styles.hint}>
              Markdown supported. These instructions guide the agent on what to do and how to report.
            </span>
          </div>

          <div className={styles.field}>
            <label className={styles.label}>Skills</label>
            {skills.length === 0 ? (
              <div className={styles.hint}>
                No skills discovered yet. Add skill sources in Settings to make reusable instructions available.
              </div>
            ) : (
              <div className={styles.checkboxGroup}>
                {skills.map((skill) => {
                  const checked = selectedSkillIds.includes(skill.id);
                  return (
                    <label key={skill.id} className={`${styles.checkboxOption} ${styles.skillOption}`}>
                      <input
                        type="checkbox"
                        checked={checked}
                        disabled={saving}
                        onChange={(e) => {
                          const nextChecked = e.target.checked;
                          setSelectedSkillIds((current) => (
                            nextChecked
                              ? [...current, skill.id]
                              : current.filter((id) => id !== skill.id)
                          ));
                        }}
                      />
                      <span className={styles.skillDetails}>
                        <span className={styles.skillHeader}>
                          <span className={styles.skillName}>{skill.name}</span>
                          <span className={styles.skillSource}>{skill.sourceName}</span>
                        </span>
                        {skill.description && (
                          <span className={styles.skillDescription}>{skill.description}</span>
                        )}
                      </span>
                    </label>
                  );
                })}
              </div>
            )}
            <span className={styles.hint}>
              Selected skills are appended to the agent instructions at runtime.
            </span>
          </div>

          <div className={styles.section}>
            <div className={styles.sectionTitle}>Execution Mode</div>
            <div className={styles.sectionDescription}>
              Agents can run on a schedule or be assigned to workspaces for manager-delegated tasks.
            </div>

            <div className={styles.field}>
              <label className={styles.label}>Scheduled Execution</label>
              <label className={styles.toggleRow}>
                <span className={styles.toggleLabel}>
                  Run this agent on a recurring schedule
                </span>
                <span className={`${styles.toggle} ${scheduleEnabled ? styles.toggleOn : ''}`}>
                  <input
                    type="checkbox"
                    checked={scheduleEnabled}
                    onChange={(e) => setScheduleEnabled(e.target.checked)}
                    disabled={saving}
                    className={styles.toggleInput}
                  />
                  <span className={styles.toggleTrack}>
                    <span className={styles.toggleThumb} />
                  </span>
                </span>
              </label>
              <span className={styles.hint}>
                {scheduleEnabled
                  ? 'When enabled, this agent can be scheduled and can also be assigned to workspace teams.'
                  : 'Workspace-only. The agent will not be registered with the scheduler but can still receive tasks from workspace managers.'}
              </span>
            </div>

            <div className={styles.field}>
              <label className={styles.label} htmlFor="agent-interval">
                Check Interval
              </label>
              <IntervalSelect
                id="agent-interval"
                value={intervalMinutes}
                onChange={setIntervalMinutes}
                disabled={saving || !scheduleEnabled}
              />
              <span className={styles.hint}>
                {scheduleEnabled
                  ? 'How often this agent runs while enabled.'
                  : 'Stored for later if you re-enable scheduling.'}
              </span>
            </div>
          </div>

          <div className={styles.field}>
            <label className={styles.label}>MCP Servers</label>
            {mcpServers.length === 0 ? (
              <div className={styles.hint}>
                No MCP servers configured yet. Add them in Settings to make external tools available to this agent.
              </div>
            ) : (
              <div className={styles.checkboxGroup}>
                {mcpServers.map((server) => {
                  const checked = selectedMcpServerIds.includes(server.id);
                  return (
                    <label key={server.id} className={styles.checkboxOption}>
                      <input
                        type="checkbox"
                        checked={checked}
                        disabled={saving || !server.enabled}
                        onChange={(e) => {
                          const nextChecked = e.target.checked;
                          setSelectedMcpServerIds((current) => (
                            nextChecked
                              ? [...current, server.id]
                              : current.filter((id) => id !== server.id)
                          ));
                        }}
                      />
                      <span>
                        {server.name}
                        {!server.enabled ? ' (disabled)' : ''}
                      </span>
                    </label>
                  );
                })}
              </div>
            )}
            <span className={styles.hint}>
              Selected servers will be attached to the agent session when it runs.
            </span>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="agent-provider-connection">
              Provider Connections <span className={styles.required}>*</span>
            </label>
            {enabledProviderConnections.length === 0 ? (
              <div className={styles.hint}>
                No enabled provider connections configured yet. Add them in Settings first.
              </div>
            ) : (
              <>
                <div className={styles.listInputRow}>
                  <select
                    id="agent-provider-connection"
                    className={styles.select}
                    value={providerConnectionDraft}
                    onChange={(e) => setProviderConnectionDraft(e.target.value)}
                    disabled={saving || availableProviderConnections.length === 0}
                  >
                    {availableProviderConnections.length === 0 ? (
                      <option value="">All enabled connections already selected</option>
                    ) : null}
                    {availableProviderConnections
                      .map((connection) => (
                        <option key={connection.id} value={connection.id}>
                          {connection.name} ({connection.modelId})
                        </option>
                      ))}
                  </select>
                  <button
                    type="button"
                    className={styles.addButton}
                    onClick={() => {
                      if (!providerConnectionDraft) return;
                      setProviderConnectionIds((current) => [...current, providerConnectionDraft]);
                      setProviderConnectionDraft('');
                    }}
                    disabled={saving || !providerConnectionDraft}
                  >
                    Add
                  </button>
                </div>
                {providerConnectionIds.length > 0 && (
                  <div className={styles.providerConnectionList}>
                    {providerConnectionIds.map((connectionId, index) => {
                      const connection = providerConnections.find((item) => item.id === connectionId);
                      const label = connection ? connection.name : connectionId;
                      const meta = index === 0 ? 'primary' : `fallback ${index}`;
                      const statusLabel = !connection
                        ? 'missing'
                        : !connection.enabled
                          ? 'disabled'
                          : null;
                      return (
                        <div key={connectionId} className={styles.providerConnectionItem}>
                          <div className={styles.providerConnectionDetails}>
                            <div className={styles.providerConnectionHeader}>
                              <span className={styles.providerConnectionName}>{label}</span>
                              <span className={styles.chipMeta}>{meta}</span>
                              {statusLabel ? (
                                <span className={styles.providerConnectionStatus}>{statusLabel}</span>
                              ) : null}
                            </div>
                            {connection ? (
                              <div className={styles.providerConnectionSubtext}>
                                {connection.providerId} · {connection.modelId}
                                {connection.accountLabel ? ` · ${connection.accountLabel}` : ''}
                              </div>
                            ) : (
                              <div className={styles.providerConnectionSubtext}>
                                This connection is no longer configured.
                              </div>
                            )}
                          </div>
                          <div className={styles.providerConnectionActions}>
                            {index > 0 && (
                              <button
                                type="button"
                                className={styles.chipRemove}
                                onClick={() => setProviderConnectionIds((current) => {
                                  const next = [...current];
                                  [next[index - 1], next[index]] = [next[index], next[index - 1]];
                                  return next;
                                })}
                                disabled={saving}
                                aria-label={`Move ${label} up`}
                                title="Move up"
                              >
                                ↑
                              </button>
                            )}
                            {index < providerConnectionIds.length - 1 && (
                              <button
                                type="button"
                                className={styles.chipRemove}
                                onClick={() => setProviderConnectionIds((current) => {
                                  const next = [...current];
                                  [next[index], next[index + 1]] = [next[index + 1], next[index]];
                                  return next;
                                })}
                                disabled={saving}
                                aria-label={`Move ${label} down`}
                                title="Move down"
                              >
                                ↓
                              </button>
                            )}
                            <button
                              type="button"
                              className={styles.chipRemove}
                              onClick={() => setProviderConnectionIds((current) => current.filter((value) => value !== connectionId))}
                              disabled={saving}
                              aria-label={`Remove ${label}`}
                              title="Remove"
                            >
                              ×
                            </button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
              </>
            )}
            <span className={styles.hint}>
              The first connection is primary. Additional connections are used as ordered fallbacks.
            </span>
          </div>

          <div className={styles.section}>
            <div className={styles.sectionTitle}>Local Capabilities</div>
            <div className={styles.sectionDescription}>
              Each agent always gets a private writable workspace folder. Use this section to grant additional local paths or shell access.
            </div>

            <div className={styles.field}>
              <label className={styles.label}>Agent Workspace</label>
              <div className={styles.fixedGrantCard}>
                <div className={styles.fixedGrantTitle}>Private agent workspace</div>
                <div className={styles.fixedGrantText}>
                  CLAI creates a dedicated workspace directory for this agent automatically. It is always available with read + write access and used as the default working directory for shell commands.
                </div>
              </div>
            </div>

            <div className={styles.field}>
              <label className={styles.label} htmlFor="agent-extra-path">
                Additional Path Grants
              </label>
              <div className={styles.listInputRow}>
                <input
                  id="agent-extra-path"
                  type="text"
                  className={styles.input}
                  value={extraPathDraft}
                  onChange={(e) => setExtraPathDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      const trimmed = extraPathDraft.trim();
                      if (!trimmed) return;
                      setExtraPathGrants((current) => {
                        if (current.some((item) => item.path === trimmed)) {
                          return current;
                        }
                        return [...current, { path: trimmed, access: extraPathAccess }];
                      });
                      setExtraPathDraft('');
                    }
                  }}
                  placeholder="$HOME, /tmp, /var/log"
                  disabled={saving}
                />
                <select
                  className={styles.selectSmall}
                  value={extraPathAccess}
                  onChange={(e) => setExtraPathAccess(e.target.value)}
                  disabled={saving}
                >
                  <option value="read_only">Read only</option>
                  <option value="read_write">Read + write</option>
                </select>
                <button
                  type="button"
                  className={styles.addButton}
                  onClick={() => {
                    const trimmed = extraPathDraft.trim();
                    if (!trimmed) return;
                    setExtraPathGrants((current) => {
                      if (current.some((item) => item.path === trimmed)) {
                        return current;
                      }
                      return [...current, { path: trimmed, access: extraPathAccess }];
                    });
                    setExtraPathDraft('');
                  }}
                  disabled={saving || !extraPathDraft.trim()}
                >
                  Add
                </button>
              </div>
              {extraPathGrants.length > 0 && (
                <div className={styles.chipList}>
                  {extraPathGrants.map((item) => (
                    <span key={item.path} className={styles.chip}>
                      <code>{item.path}</code>
                      <span className={styles.chipMeta}>{item.access === 'read_write' ? 'rw' : 'ro'}</span>
                      <button
                        type="button"
                        className={styles.chipRemove}
                        onClick={() => setExtraPathGrants((current) => current.filter((grant) => grant.path !== item.path))}
                        disabled={saving}
                        aria-label={`Remove ${item.path}`}
                      >
                        ×
                      </button>
                    </span>
                  ))}
                </div>
              )}
              <span className={styles.hint}>
                Use this for extra locations such as <code>$HOME</code> as read-only or <code>/tmp</code> as read + write.
              </span>
            </div>

            <div className={styles.field}>
              <label className={styles.label} htmlFor="agent-shell-mode">
                Shell Access
              </label>
              <select
                id="agent-shell-mode"
                className={styles.select}
                value={shellMode}
                onChange={(e) => setShellMode(e.target.value)}
                disabled={saving}
              >
                <option value="off">Off</option>
                <option value="restricted">Restricted</option>
                <option value="full">Full</option>
              </select>
              <span className={styles.hint}>
                {shellMode === 'off' && 'The agent cannot run shell commands.'}
                {shellMode === 'restricted' && 'Only explicitly allowed commands can run. Blocked list always takes priority.'}
                {shellMode === 'full' && 'Any command is allowed except those explicitly blocked.'}
              </span>
            </div>

            {shellMode === 'restricted' && (
              <div className={styles.gridTwo}>
                <div className={styles.field}>
                  <ListInputField
                    id="agent-shell-allow"
                    label="Allowed Commands"
                    value={allowedCommandDraft}
                    onChange={setAllowedCommandDraft}
                    onAdd={() => {
                      setAllowedCommands((current) => addUniqueItem(current, allowedCommandDraft));
                      setAllowedCommandDraft('');
                    }}
                    items={allowedCommands}
                    onRemove={(item) => setAllowedCommands((current) => current.filter((value) => value !== item))}
                    placeholder="e.g. kubectl get"
                    disabled={saving}
                    hint={'Prefix match \u2014 e.g. "kubectl get" allows "kubectl get pods". Empty list means no commands allowed.'}
                  />
                </div>

                <div className={styles.field}>
                  <ListInputField
                    id="agent-shell-block"
                    label="Blocked Commands"
                    value={blockedCommandDraft}
                    onChange={setBlockedCommandDraft}
                    onAdd={() => {
                      setBlockedCommands((current) => addUniqueItem(current, blockedCommandDraft));
                      setBlockedCommandDraft('');
                    }}
                    items={blockedCommands}
                    onRemove={(item) => setBlockedCommands((current) => current.filter((value) => value !== item))}
                    placeholder="e.g. rm"
                    disabled={saving}
                    variant="danger"
                    hint="Prefix match. Blocked always wins over allowed."
                  />
                </div>
              </div>
            )}

            {shellMode === 'full' && (
              <ListInputField
                id="agent-shell-block-full"
                label="Blocked Commands"
                value={blockedCommandDraft}
                onChange={setBlockedCommandDraft}
                onAdd={() => {
                  setBlockedCommands((current) => addUniqueItem(current, blockedCommandDraft));
                  setBlockedCommandDraft('');
                }}
                items={blockedCommands}
                onRemove={(item) => setBlockedCommands((current) => current.filter((value) => value !== item))}
                placeholder="e.g. rm"
                disabled={saving}
                variant="danger"
                hint={'Prefix match \u2014 e.g. "rm" blocks "rm -rf". These commands will always be rejected.'}
              />
            )}

            <div className={styles.field}>
              <label className={styles.label}>Web Access</label>
              <label className={styles.toggleRow}>
                <span className={styles.toggleLabel}>
                  Allow web search and page fetching
                </span>
                <span className={`${styles.toggle} ${webEnabled ? styles.toggleOn : ''}`}>
                  <input
                    type="checkbox"
                    checked={webEnabled}
                    onChange={(e) => setWebEnabled(e.target.checked)}
                    disabled={saving}
                    className={styles.toggleInput}
                  />
                  <span className={styles.toggleTrack}>
                    <span className={styles.toggleThumb} />
                  </span>
                </span>
              </label>
              <span className={styles.hint}>
                Enables <code>web.search</code> (DuckDuckGo) and <code>web.fetch</code> (read any public URL as markdown).
              </span>
            </div>
          </div>

          {/* Actions */}
          <div className={styles.actions}>
            <button
              type="button"
              className={styles.cancelButton}
              onClick={onClose}
              disabled={saving}
            >
              Cancel
            </button>
            <button
              type="submit"
              className={styles.submitButton}
              disabled={saving}
            >
              {saving ? (
                <>
                  <LoadingIcon />
                  <span>Saving...</span>
                </>
              ) : (
                <span>{isEditing ? 'Save Changes' : 'Create Agent'}</span>
              )}
            </button>
          </div>
        </form>
      </div>
    </div>,
    document.body
  );
};

export default AgentFormModal;
