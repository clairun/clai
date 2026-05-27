/**
 * ContextBadge Component
 *
 * Displays individual context items as clean, minimalistic badges.
 * Used within the ContextPanel to show space, room, and custom key-value pairs.
 */

import React from 'react';
import styles from './ContextBadge.module.css';

/**
 * SVG Icon Components - Simple, monochromatic icons that inherit color
 */
const SpaceIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="12" cy="12" r="10" />
    <line x1="2" y1="12" x2="22" y2="12" />
    <path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z" />
  </svg>
);

const RoomIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <path d="M9 3v18" />
    <circle cx="7" cy="12" r="0.5" fill="currentColor" />
  </svg>
);

const CustomIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="12" cy="12" r="3" />
    <path d="M12 1v6m0 6v6m9-9h-6m-6 0H3" />
  </svg>
);

const CreditsIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="12" cy="12" r="10" />
    <path d="M16 8h-6a2 2 0 1 0 0 4h4a2 2 0 1 1 0 4H8" />
    <path d="M12 18V6" />
  </svg>
);

const McpIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M8 6h8" />
    <path d="M8 12h8" />
    <path d="M8 18h8" />
    <path d="M4 6h.01" />
    <path d="M4 12h.01" />
    <path d="M4 18h.01" />
    <rect x="2" y="3" width="20" height="18" rx="3" />
  </svg>
);

type BadgeType = 'space' | 'room' | 'credits' | 'mcp' | 'custom';
type BadgeVariant = 'warning' | 'danger' | 'disabled' | 'add';

interface ContextBadgeProps {
  /** Type of badge: 'space', 'room', 'credits', 'mcp', or 'custom' */
  type?: BadgeType;
  /** Label to display (for custom badges, this is the key) */
  label?: string;
  /** Value to display */
  value?: string;
  /** Optional click handler for interactive badges */
  onClick?: () => void;
  /** Whether the badge is clickable (default: false) */
  clickable?: boolean;
  /** Optional variant for styling */
  variant?: BadgeVariant;
  titleOverride?: string;
  iconElement?: React.ReactNode;
}

const ContextBadge = ({
  type = 'custom',
  label,
  value,
  onClick,
  clickable = false,
  variant,
  titleOverride,
  iconElement,
}: ContextBadgeProps) => {
  // Determine badge style based on type and variant
  const typeClass = type.charAt(0).toUpperCase() + type.slice(1);
  const variantClass = variant ? variant.charAt(0).toUpperCase() + variant.slice(1) : '';
  const badgeClass = `${styles.badge} ${styles[`badge${typeClass}${variantClass}`] || styles[`badge${typeClass}`] || ''} ${clickable ? styles.clickable : ''}`;

  // Select the appropriate icon based on type
  const IconComponent = type === 'space'
    ? SpaceIcon
    : type === 'room'
      ? RoomIcon
      : type === 'credits'
        ? CreditsIcon
        : type === 'mcp'
          ? McpIcon
          : CustomIcon;

  // For space, room, and credits, we show just the value
  // For custom, we show key=value
  const displayText = type === 'space' || type === 'room' || type === 'credits' || type === 'mcp'
    ? value
    : `${label}=${value}`;

  // Handle click if clickable
  const handleClick = (e: React.SyntheticEvent) => {
    if (clickable && onClick) {
      e.stopPropagation();
      onClick();
    }
  };

  return (
    <div
      className={badgeClass}
      title={titleOverride || `${label}: ${value}`}
      onClick={handleClick}
      role={clickable ? 'button' : undefined}
      tabIndex={clickable ? 0 : undefined}
      onKeyDown={clickable ? (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          handleClick(e);
        }
      } : undefined}
    >
      <span className={styles.icon}>
        {iconElement || <IconComponent />}
      </span>
      <span className={styles.text}>{displayText}</span>
    </div>
  );
};

export default ContextBadge;
