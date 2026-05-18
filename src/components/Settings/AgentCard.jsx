/**
 * AgentCard Component
 *
 * Displays a single agent with its details and action buttons.
 */

import React, { useState } from 'react';
import styles from './AgentCard.module.css';

/**
 * Edit icon
 */
const EditIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7" />
    <path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z" />
  </svg>
);

/**
 * Delete icon
 */
const DeleteIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <polyline points="3 6 5 6 21 6" />
    <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
  </svg>
);

/**
 * Loading spinner
 */
const LoadingIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className={styles.spinner}>
    <circle cx="12" cy="12" r="10" opacity="0.25" />
    <path d="M12 2a10 10 0 0 1 10 10" />
  </svg>
);

const PowerIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M12 2v10" />
    <path d="M18.4 5.6a9 9 0 1 1-12.8 0" />
  </svg>
);

/**
 * Format interval for display
 */
const formatInterval = (minutes) => {
  if (minutes < 60) {
    return `${minutes} min`;
  }
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  if (remainingMinutes === 0) {
    return hours === 1 ? '1 hour' : `${hours} hours`;
  }
  return `${hours}h ${remainingMinutes}m`;
};

/**
 * Truncate description for preview
 */
const truncateDescription = (text, maxLength = 120) => {
  if (!text) return '';
  // Remove markdown syntax for preview
  const plainText = text
    .replace(/#{1,6}\s/g, '') // Remove headers
    .replace(/\*\*([^*]+)\*\*/g, '$1') // Remove bold
    .replace(/\*([^*]+)\*/g, '$1') // Remove italic
    .replace(/`([^`]+)`/g, '$1') // Remove code
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1') // Remove links
    .replace(/\n+/g, ' ') // Replace newlines with spaces
    .trim();

  if (plainText.length <= maxLength) return plainText;
  return plainText.substring(0, maxLength).trim() + '...';
};

/**
 * AgentCard - Individual agent display
 *
 * @param {Object} props
 * @param {Object} props.agent - The agent data
 * @param {Function} props.onEdit - Callback when edit is clicked
 * @param {Function} props.onDelete - Callback when delete is clicked
 * @param {Function} props.onToggleEnabled - Callback when enable state toggles
 * @param {boolean} props.isDeleting - Whether deletion is in progress
 * @param {boolean} props.isToggling - Whether enable/disable is in progress
 */
const AgentCard = ({ agent, mcpServers = [], skills = [], onEdit, onDelete, onToggleEnabled, isDeleting, isToggling }) => {
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);

  const handleDeleteClick = () => {
    setShowDeleteConfirm(true);
  };

  const handleConfirmDelete = () => {
    setShowDeleteConfirm(false);
    onDelete();
  };

  const handleCancelDelete = () => {
    setShowDeleteConfirm(false);
  };

  const isEnabled = !!agent.enabled;
  const selectedMcpServerNames = (agent.selectedMcpServerIds || [])
    .map((id) => mcpServers.find((server) => server.id === id)?.name)
    .filter(Boolean);
  const selectedSkillNames = (agent.selectedSkillIds || [])
    .map((id) => skills.find((skill) => skill.id === id)?.name)
    .filter(Boolean);
  const filesystemMode = agent.execution?.filesystem?.mode || 'off';
  const shellMode = agent.execution?.shell?.mode || 'off';
  const scheduleLabel = agent.scheduleEnabled === false ? 'On demand' : `Every ${formatInterval(agent.intervalMinutes)}`;
  const exposedToolCount = agent.exposedTools?.length || 0;

  return (
    <div className={styles.card}>
      <div className={styles.cardContent}>
        <div className={styles.cardHeader}>
          <h4 className={styles.agentName}>{agent.name}</h4>
          <span className={styles.interval}>
            {scheduleLabel}
          </span>
          <button
            className={`${styles.statusToggle} ${isEnabled ? styles.statusToggleOn : styles.statusToggleOff}`}
            onClick={onToggleEnabled}
            disabled={isToggling}
            title={isEnabled ? 'Disable agent' : 'Enable agent'}
          >
            {isToggling ? <LoadingIcon /> : <PowerIcon />}
            <span>{isEnabled ? 'Enabled' : 'Disabled'}</span>
          </button>
        </div>

        {agent.description && (
          <p className={styles.description}>
            {truncateDescription(agent.description)}
          </p>
        )}

        <p className={styles.description}>
          MCP: {selectedMcpServerNames.length > 0 ? selectedMcpServerNames.join(', ') : 'None'}
        </p>
        <p className={styles.description}>
          Skills: {selectedSkillNames.length > 0 ? selectedSkillNames.join(', ') : 'None'}
        </p>
        {exposedToolCount > 0 && (
          <p className={styles.description}>
            Legacy exposed tools: {exposedToolCount}
          </p>
        )}
        <p className={styles.description}>
          Local: FS {filesystemMode.replace('_', ' ')} · Shell {shellMode.replace('_', ' ')}
        </p>
      </div>

      <div className={styles.cardActions}>
        {showDeleteConfirm ? (
          <div className={styles.confirmDelete}>
            <span className={styles.confirmText}>Delete?</span>
            <button
              className={styles.confirmYes}
              onClick={handleConfirmDelete}
              disabled={isDeleting}
            >
              {isDeleting ? <LoadingIcon /> : 'Yes'}
            </button>
            <button
              className={styles.confirmNo}
              onClick={handleCancelDelete}
              disabled={isDeleting}
            >
              No
            </button>
          </div>
        ) : (
          <>
            <button
              className={styles.actionButton}
              onClick={onEdit}
              title="Edit agent"
            >
              <EditIcon />
            </button>
            <button
              className={`${styles.actionButton} ${styles.deleteButton}`}
              onClick={handleDeleteClick}
              title="Delete agent"
            >
              <DeleteIcon />
            </button>
          </>
        )}
      </div>

    </div>
  );
};

export default AgentCard;
