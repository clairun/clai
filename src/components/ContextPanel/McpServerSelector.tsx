import React, { useEffect, useRef, useState } from 'react';
import type { McpServerResponse } from '../../generated/bindings';
import McpServerAvatar from './McpServerAvatar';
import styles from './McpServerSelector.module.css';

interface McpServerSelectorProps {
  servers: McpServerResponse[];
  attachedIds: string[];
  disabledIds: string[];
  onAdd: (serverId: string) => void | Promise<void>;
  onRemove: (serverId: string) => void | Promise<void>;
  onClose: () => void;
}

const McpServerSelector = ({
  servers,
  attachedIds,
  disabledIds,
  onAdd,
  onRemove,
  onClose,
}: McpServerSelectorProps) => {
  const selectorRef = useRef<HTMLDivElement>(null);
  const [busyServerId, setBusyServerId] = useState<string | null>(null);

  useEffect(() => {
    const rafId = requestAnimationFrame(() => {
      const firstButton = selectorRef.current?.querySelector('button');
      firstButton?.focus();
    });

    return () => cancelAnimationFrame(rafId);
  }, []);

  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (selectorRef.current && !selectorRef.current.contains(event.target as Node)) {
        onClose();
      }
    };

    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        onClose();
      }
    };

    const timeoutId = setTimeout(() => {
      document.addEventListener('mousedown', handleClickOutside);
      document.addEventListener('keydown', handleEscape);
    }, 100);

    return () => {
      clearTimeout(timeoutId);
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [onClose]);

  const handleAdd = async (serverId: string) => {
    setBusyServerId(serverId);
    try {
      await onAdd(serverId);
    } finally {
      setBusyServerId(null);
    }
  };

  const handleRemove = async (serverId: string) => {
    setBusyServerId(serverId);
    try {
      await onRemove(serverId);
    } finally {
      setBusyServerId(null);
    }
  };

  return (
    <div className={styles.overlay}>
      <div ref={selectorRef} className={styles.selector}>
        <div className={styles.header}>
          <div>
            <h3 className={styles.title}>MCP Servers</h3>
            <p className={styles.subtitle}>
              Add MCP servers to this tab. Click an attached MCP badge in the context bar to toggle it on or off.
            </p>
          </div>
          <button
            className={styles.closeButton}
            onClick={onClose}
            aria-label="Close MCP server selector"
          >
            ×
          </button>
        </div>

        <div className={styles.itemsList}>
          {servers.length === 0 ? (
            <div className={styles.emptyState}>
              No enabled MCP servers are configured in Settings.
            </div>
          ) : (
            servers.map((server) => {
              const isAttached = attachedIds.includes(server.id);
              const isDisabled = disabledIds.includes(server.id);
              const transportLabel = server.transport?.type === 'http'
                ? 'HTTP'
                : server.transport?.type === 'stdio'
                  ? 'STDIO'
                  : 'MCP';

              return (
                <div
                  key={server.id}
                  className={`${styles.item} ${isAttached ? styles.itemSelected : ''}`}
                >
                  <div className={styles.itemMain}>
                    <McpServerAvatar server={server} disabled={isDisabled} />
                    <div className={styles.itemCopy}>
                      <div className={styles.itemHeader}>
                        <div className={styles.itemName}>{server.name}</div>
                        {isAttached && (
                          <span className={`${styles.statusPill} ${isDisabled ? styles.statusDisabled : styles.statusActive}`}>
                            {isDisabled ? 'Disabled' : 'Active'}
                          </span>
                        )}
                      </div>
                      <div className={styles.itemMeta}>{transportLabel}</div>
                    </div>
                    <div className={styles.itemActions}>
                      {isAttached ? (
                        <button
                          type="button"
                          className={styles.removeButton}
                          onClick={() => handleRemove(server.id)}
                          disabled={busyServerId === server.id}
                        >
                          Remove
                        </button>
                      ) : (
                        <button
                          type="button"
                          className={styles.addButton}
                          onClick={() => handleAdd(server.id)}
                          disabled={busyServerId === server.id}
                        >
                          Add
                        </button>
                      )}
                    </div>
                  </div>
                </div>
              );
            })
          )}
        </div>

        <div className={styles.footer}>
          <div className={styles.footerHint}>
            Attached servers stay on the tab until you remove them here.
          </div>
          <button
            type="button"
            className={styles.secondaryButton}
            onClick={onClose}
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
};

export default McpServerSelector;
