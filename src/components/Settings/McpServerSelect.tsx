/**
 * MCP servers as a dropdown of checkboxes. The trigger names what is on,
 * so the choice reads at a glance without opening it; the popover is a
 * plain checkbox list. Escape closes the popover only (it joins the
 * overlay stack), and focus goes back to the trigger.
 *
 * The popover renders right after its trigger, not in a portal: it stays
 * inside the host dialog, and Tab order runs trigger → popover → the next
 * control, so Shift+Tab reaches the trigger and Tab past the end closes it.
 */

import React, { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { useOverlayLayer } from '../../hooks/useOverlayLayer';
import { openGlobalSettings } from '../../utils/globalSettings';
import styles from './McpServerSelect.module.css';

export interface McpServerOption {
  id: string;
  name: string;
  description?: string | null;
}

/** "github, netdata +2" — the first names, then how many more. */
export const summarizeNames = (names: string[], shown = 2): string => {
  if (names.length === 0) return 'None';
  const head = names.slice(0, shown).join(', ');
  return names.length > shown ? `${head} +${names.length - shown}` : head;
};

const McpServerSelect = ({
  servers,
  selectedIds,
  onChange,
  disabled,
  labelId,
}: {
  servers: McpServerOption[];
  selectedIds: string[];
  onChange: (next: string[]) => void;
  disabled?: boolean;
  /** Id of the visible label naming this control. */
  labelId: string;
}) => {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<React.CSSProperties>({});
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const popoverId = useId();

  const selected = servers.filter((server) => selectedIds.includes(server.id));
  // Ids saved on the agent for servers that no longer exist stay in the
  // payload untouched; they are simply not offered here.
  const empty = servers.length === 0;

  const close = useCallback((refocus: boolean) => {
    setOpen(false);
    if (refocus) triggerRef.current?.focus();
  }, []);

  useOverlayLayer(open, () => close(true), { lockScroll: false });

  useLayoutEffect(() => {
    if (!open || !triggerRef.current) return;
    const rect = triggerRef.current.getBoundingClientRect();
    const width = Math.max(rect.width, 280);
    const below = window.innerHeight - rect.bottom;
    const above = below < 320 && rect.top > below;
    setPosition({
      position: 'fixed',
      left: Math.min(rect.left, window.innerWidth - width - 8),
      width,
      ...(above ? { bottom: window.innerHeight - rect.top + 4 } : { top: rect.bottom + 4 }),
    });
  }, [open]);

  useEffect(() => {
    if (!open) return undefined;
    popoverRef.current?.querySelector<HTMLInputElement>('input[type="checkbox"]')?.focus();
    const onPointer = (event: MouseEvent) => {
      const target = event.target as Node;
      if (popoverRef.current?.contains(target) || triggerRef.current?.contains(target)) return;
      close(false);
    };
    document.addEventListener('mousedown', onPointer);
    return () => document.removeEventListener('mousedown', onPointer);
  }, [open, close]);

  const toggle = (id: string, on: boolean) => {
    onChange(on ? [...selectedIds, id] : selectedIds.filter((other) => other !== id));
  };

  // Focus leaving both the popover and its trigger closes it (Tab past the end).
  const handleBlur = (event: React.FocusEvent) => {
    const next = event.relatedTarget as Node | null;
    if (next && (popoverRef.current?.contains(next) || triggerRef.current?.contains(next))) return;
    if (next) close(false);
  };

  const moveFocus = (event: React.KeyboardEvent) => {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
    const boxes = Array.from(popoverRef.current?.querySelectorAll<HTMLInputElement>('input[type="checkbox"]') ?? []);
    const at = boxes.indexOf(document.activeElement as HTMLInputElement);
    if (at < 0) return;
    event.preventDefault();
    boxes[(at + (event.key === 'ArrowDown' ? 1 : boxes.length - 1)) % boxes.length]?.focus();
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className={`${styles.trigger} ${open ? styles.triggerOpen : ''}`}
        onClick={() => (open ? close(false) : setOpen(true))}
        disabled={disabled || empty}
        aria-haspopup="true"
        aria-expanded={open}
        aria-controls={open ? popoverId : undefined}
        aria-labelledby={`${labelId} ${popoverId}-value`}
      >
        <span id={`${popoverId}-value`} className={styles.value}>
          {empty ? 'None configured' : summarizeNames(selected.map((server) => server.name))}
        </span>
        {!empty && <span className={styles.count} aria-hidden="true">{selected.length}/{servers.length}</span>}
        <svg className={styles.chevron} width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <polyline points="6 9 12 15 18 9" />
        </svg>
      </button>

      {open && (
        <div
          ref={popoverRef}
          id={popoverId}
          className={styles.popover}
          style={position}
          role="group"
          aria-labelledby={labelId}
          onBlur={handleBlur}
          onKeyDown={moveFocus}
        >
          <div className={styles.popoverHeader}>
            <span>{selected.length} of {servers.length} on</span>
            {selected.length > 0 && (
              <button type="button" className={styles.clear} onClick={() => onChange(selectedIds.filter((id) => !servers.some((s) => s.id === id)))}>
                Turn all off
              </button>
            )}
          </div>
          <ul className={styles.options}>
            {servers.map((server) => {
              const on = selectedIds.includes(server.id);
              return (
                <li key={server.id}>
                  <label className={`${styles.option} ${on ? styles.optionOn : ''}`}>
                    <input
                      type="checkbox"
                      checked={on}
                      onChange={(e) => toggle(server.id, e.target.checked)}
                      disabled={disabled}
                    />
                    <span className={styles.optionText}>
                      <span className={styles.optionName}>{server.name}</span>
                      {server.description && <span className={styles.optionDescription}>{server.description}</span>}
                    </span>
                  </label>
                </li>
              );
            })}
          </ul>
          <div className={styles.popoverFooter}>
            <button
              type="button"
              className={styles.manage}
              onClick={() => {
                close(false);
                openGlobalSettings({ tab: 'mcp_servers' });
              }}
            >
              Manage servers…
            </button>
          </div>
        </div>
      )}
    </>
  );
};

export default McpServerSelect;
