/**
 * AgentsSettings Component
 *
 * Displays and manages user-defined automations.
 * Allows creating, editing, and deleting agents.
 */

import React, { useState, useEffect } from 'react';
import {
  getAgents,
  createAgent,
  updateAgent,
  deleteAgent,
  getMcpServers,
  getSkills,
  setAgentEnabled,
} from '../../api/client';
import { assistantClient } from '../../assistant';
import AgentCard from './AgentCard';
import AgentFormModal from './AgentFormModal';
import styles from './AgentsSettings.module.css';

/**
 * Loading spinner icon
 */
const LoadingIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className={styles.spinner}>
    <circle cx="12" cy="12" r="10" opacity="0.25" />
    <path d="M12 2a10 10 0 0 1 10 10" />
  </svg>
);

/**
 * Plus icon for add button
 */
const PlusIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="12" y1="5" x2="12" y2="19" />
    <line x1="5" y1="12" x2="19" y2="12" />
  </svg>
);

/**
 * Warning icon
 */
const WarningIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="12" cy="12" r="10" />
    <line x1="12" y1="8" x2="12" y2="12" />
    <line x1="12" y1="16" x2="12.01" y2="16" />
  </svg>
);

/**
 * AgentsSettings - Automation management interface
 */
const AgentsSettings = () => {
  const [agents, setAgents] = useState([]);
  const [mcpServers, setMcpServers] = useState([]);
  const [skills, setSkills] = useState([]);
  const [providerConnections, setProviderConnections] = useState([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [editingAgent, setEditingAgent] = useState(null);
  const [deletingId, setDeletingId] = useState(null);
  const [togglingId, setTogglingId] = useState(null);

  // Fetch automations and MCP servers on mount
  useEffect(() => {
    fetchData();
  }, []);

  const fetchData = async () => {
    setLoading(true);
    setError(null);

    try {
      const [agentsResult, mcpServersResult, providerConnectionsResult] = await Promise.all([
        getAgents(),
        getMcpServers(),
        assistantClient.listProviderConnections(),
      ]);
      setAgents(agentsResult || []);
      setMcpServers(mcpServersResult || []);
      setProviderConnections(providerConnectionsResult || []);

      try {
        setSkills((await getSkills()) || []);
      } catch (skillsError) {
        console.warn('[AgentsSettings] Failed to fetch skills:', skillsError);
        setSkills([]);
      }
    } catch (err) {
      console.error('[AgentsSettings] Failed to fetch data:', err);
      setError('Failed to load agents. Please try again.');
    } finally {
      setLoading(false);
    }
  };

  const fetchAgents = async () => {
    try {
      const result = await getAgents();
      setAgents(result || []);
    } catch (err) {
      console.error('[AgentsSettings] Failed to fetch agents:', err);
      setError('Failed to load agents. Please try again.');
    }
  };

  // Handle creating a new agent
  const handleCreate = () => {
    setEditingAgent(null);
    setIsFormOpen(true);
  };

  // Handle editing an agent
  const handleEdit = (agent) => {
    setEditingAgent(agent);
    setIsFormOpen(true);
  };

  // Handle deleting an agent
  const handleDelete = async (agentId) => {
    if (deletingId) return; // Prevent double-clicks

    setDeletingId(agentId);
    setError(null);

    try {
      await deleteAgent(agentId);
      setAgents(agents.filter(a => a.id !== agentId));
    } catch (err) {
      console.error('[AgentsSettings] Failed to delete agent:', err);
      setError('Failed to delete agent. Please try again.');
    } finally {
      setDeletingId(null);
    }
  };

  // Handle form submission (create or update)
  const handleFormSubmit = async (formData) => {
    setError(null);

    try {
      if (editingAgent) {
        // Update existing agent
        const updated = await updateAgent({
          id: editingAgent.id,
          ...formData,
        });
        setAgents(agents.map(a => a.id === updated.id ? updated : a));
      } else {
        // Create new agent
        const created = await createAgent(formData);
        setAgents([...agents, created]);
      }
      setIsFormOpen(false);
      setEditingAgent(null);
    } catch (err) {
      console.error('[AgentsSettings] Failed to save agent:', err);
      throw err; // Re-throw so form can show error
    }
  };

  const handleToggleEnabled = async (agent) => {
    if (togglingId) return;

    setTogglingId(agent.id);
    setError(null);

    try {
      const updated = await setAgentEnabled(agent.id, !agent.enabled);
      setAgents(agents.map(a => a.id === updated.id ? updated : a));
    } catch (err) {
      console.error('[AgentsSettings] Failed to toggle agent:', err);
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to update agent status. Please try again.'));
    } finally {
      setTogglingId(null);
    }
  };

  // Handle form close
  const handleFormClose = () => {
    setIsFormOpen(false);
    setEditingAgent(null);
  };

  // Loading state
  if (loading) {
    return (
      <div className={styles.container}>
        <div className={styles.loadingState}>
          <LoadingIcon />
          <span>Loading agents...</span>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <div className={styles.headerText}>
          <h3 className={styles.title}>Agents</h3>
          <p className={styles.description}>
            Create reusable agents with custom instructions, skills, provider defaults, and local capabilities.
          </p>
        </div>
        <button className={styles.addButton} onClick={handleCreate}>
          <PlusIcon />
          <span>Add Agent</span>
        </button>
      </div>

      {error && (
        <div className={styles.errorBanner}>
          <WarningIcon />
          <span>{error}</span>
        </div>
      )}

      {agents.length === 0 ? (
        <div className={styles.emptyState}>
          <div className={styles.emptyIcon}>
            <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
              <rect x="3" y="11" width="18" height="10" rx="2" />
              <circle cx="12" cy="5" r="2" />
              <path d="M12 7v4" />
              <circle cx="8" cy="16" r="1" fill="currentColor" />
              <circle cx="16" cy="16" r="1" fill="currentColor" />
            </svg>
          </div>
          <h4 className={styles.emptyTitle}>No agents configured</h4>
          <p className={styles.emptyDescription}>
            Create your first reusable agent definition for scheduled work or workspace teams.
          </p>
          <button className={styles.emptyButton} onClick={handleCreate}>
            <PlusIcon />
            <span>Create Agent</span>
          </button>
        </div>
      ) : (
        <div className={styles.agentList}>
          {agents.map((agent) => (
            <AgentCard
              key={agent.id}
              agent={agent}
              mcpServers={mcpServers}
              skills={skills}
              onEdit={() => handleEdit(agent)}
              onDelete={() => handleDelete(agent.id)}
              onToggleEnabled={() => handleToggleEnabled(agent)}
              isDeleting={deletingId === agent.id}
              isToggling={togglingId === agent.id}
            />
          ))}
        </div>
      )}

      <div className={styles.hint}>
        <p>
          Enabled agents can run on a schedule or be assigned to workspaces. Workspace managers delegate tasks to assigned agents inside that workspace boundary.
        </p>
      </div>

      {/* Agent Form Modal */}
      <AgentFormModal
        isOpen={isFormOpen}
        onClose={handleFormClose}
        onSubmit={handleFormSubmit}
        agent={editingAgent}
        mcpServers={mcpServers}
        skills={skills}
        providerConnections={providerConnections}
      />
    </div>
  );
};

export default AgentsSettings;
